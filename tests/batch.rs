#![cfg(all(feature = "derive", feature = "test-util"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use typesafeai_sdk_community::batch::{Pacer, Progress};
use typesafeai_sdk_community::testing::{MockResponse, MockTransport};
use typesafeai_sdk_community::{Answer, NoulAnswer, Questions, RetryPolicy, TypeSafeClient};

#[derive(Debug, Questions)]
struct Triage {
    #[noul("Billing?")]
    billing: NoulAnswer,
}

/// A transport that answers every request after a delay, tracking peak concurrency.
#[derive(Clone)]
struct SlowMock {
    inner: MockTransport,
    delay: Duration,
    in_flight: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

impl typesafeai_sdk_community::transport::Transport for SlowMock {
    fn send(
        &self,
        request: typesafeai_sdk_community::transport::HttpRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = typesafeai_sdk_community::Result<typesafeai_sdk_community::transport::HttpResponse>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            let result = self.inner.send(request).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            result
        })
    }
}

fn slow_client(mock: &MockTransport, delay: Duration) -> (TypeSafeClient, Arc<AtomicUsize>) {
    let peak = Arc::new(AtomicUsize::new(0));
    let slow = SlowMock { inner: mock.clone(), delay, in_flight: Arc::default(), peak: Arc::clone(&peak) };
    let client = TypeSafeClient::builder().api_key("k").retry(RetryPolicy::none()).transport(slow).build().unwrap();
    (client, peak)
}

#[tokio::test]
async fn batch_aligns_results_and_bounds_concurrency() {
    let mock = MockTransport::new();
    // The mock serves in request order; with a fixed delay, request order is start order.
    for i in 0..10 {
        mock.enqueue(MockResponse::answers().noul("billing", i as f64 / 10.0).usage(10, 1));
    }
    let (client, peak) = slow_client(&mock, Duration::from_millis(30));
    let seen = Arc::new(Mutex::new(Vec::<Progress>::new()));
    let progress = Arc::clone(&seen);

    let started = Instant::now();
    let outcome = client
        .batch::<Triage>((0..10).map(|i| json!({"ticket": i})))
        .concurrency(3)
        .on_progress(move |p| progress.lock().unwrap().push(p))
        .run()
        .await;

    assert_eq!(outcome.succeeded(), 10);
    assert_eq!(outcome.failed(), 0);
    assert_eq!(outcome.usage.input_tokens, Some(100));
    assert_eq!(outcome.usage.output_tokens, Some(10));
    assert!(peak.load(Ordering::SeqCst) <= 3, "peak concurrency {}", peak.load(Ordering::SeqCst));
    assert!(peak.load(Ordering::SeqCst) >= 2);
    // 10 items, 3 at a time, 30ms each: at least 4 rounds.
    assert!(started.elapsed() >= Duration::from_millis(100), "{:?}", started.elapsed());

    // Every result sits at its input index and carries its own state's request.
    for (index, answered) in outcome.ok() {
        let request = &mock.requests()[index];
        assert_eq!(request.state().unwrap()["ticket"], json!(index));
        assert!(answered.billing.noul >= 0.0);
    }
    let events = seen.lock().unwrap();
    assert_eq!(events.len(), 10);
    assert_eq!(events.last().unwrap().completed, 10);
    assert!(events.iter().all(|e| e.total == 10 && e.ok));
}

#[tokio::test]
async fn batch_keeps_failures_in_place() {
    let mock = MockTransport::new();
    mock.enqueue(MockResponse::answers().noul("billing", 0.1))
        .enqueue(MockResponse::error(422, "bad state"))
        .enqueue(MockResponse::answers().noul("billing", 0.3));
    let outcome = mock.client().batch::<Triage>(["a", "b", "c"]).concurrency(1).run().await;
    assert_eq!(outcome.succeeded(), 2);
    let errors: Vec<usize> = outcome.errors().map(|(i, _)| i).collect();
    assert_eq!(errors, [1]);
    assert_eq!(outcome.results[0].as_ref().unwrap().billing.noul, 0.1);
    assert_eq!(outcome.results[2].as_ref().unwrap().billing.noul, 0.3);
    assert!(outcome.into_all().is_err());

    let outcome = mock.client().batch::<Triage>(Vec::<String>::new()).run().await;
    assert_eq!(outcome.results.len(), 0);
    assert_eq!(outcome.usage, typesafeai_sdk_community::Usage::default());
}

#[tokio::test]
async fn batch_options_reach_every_request() {
    let mock = MockTransport::new();
    mock.fallback(MockResponse::answers().noul("billing", 0.5).noul("extra", 0.9));
    let outcome = mock
        .client()
        .batch::<Triage>(["a", "b"])
        .model("jev-pinned")
        .header("x-batch", "1")
        .question("extra", typesafeai_sdk_community::Noul::new("Extra?"))
        .run()
        .await;
    assert_eq!(outcome.succeeded(), 2);
    for request in mock.requests() {
        assert_eq!(request.model(), Some("jev-pinned"));
        assert_eq!(request.header("x-batch"), Some("1"));
        assert!(request.question("extra").is_some());
    }
    assert_eq!(outcome.results[0].as_ref().unwrap().response.noul("extra").unwrap().noul, 0.9);

    let outcome = mock.client().batch::<Triage>(["a"]).header("bad name", "v").run().await;
    assert_eq!(outcome.failed(), 1);
    assert!(outcome.results[0].as_ref().unwrap_err().to_string().contains("Invalid header name"));
}

#[tokio::test]
async fn shared_pacer_pauses_the_whole_batch() {
    let mock = MockTransport::new();
    // First request is rate limited with a 150ms retry-after; everything else succeeds.
    mock.enqueue(MockResponse::rate_limited(Duration::from_millis(150)));
    mock.fallback(MockResponse::answers().noul("billing", 1.0));
    let client = TypeSafeClient::builder()
        .api_key("k")
        .transport(mock.clone())
        .retry(RetryPolicy::default().backoff_initial(Duration::from_millis(1)).backoff_max(Duration::from_millis(1)))
        .build()
        .unwrap();
    let pacer = Pacer::new();
    let started = Instant::now();
    let outcome = client.batch::<Triage>(["a", "b", "c", "d"]).concurrency(4).pacer(pacer.clone()).run().await;
    assert_eq!(outcome.succeeded(), 4);
    // The pause applied to every task, not only the one that saw the 429.
    assert!(started.elapsed() >= Duration::from_millis(150), "{:?}", started.elapsed());
    assert_eq!(mock.request_count(), 5);
    assert_eq!(pacer.remaining(), Duration::ZERO);
}

#[tokio::test]
async fn rerank_orders_candidates() {
    let mock = MockTransport::new();
    // Served in request order = candidate order (concurrency 1).
    for p in [0.2, 0.9, 0.5] {
        mock.enqueue(MockResponse::answers().noul("relevance", p));
    }
    let candidates = vec!["about refunds", "how to get a refund", "shipping times"];
    let ranked = mock.client().rerank("How do I get a refund?", candidates.clone()).concurrency(1).run().await.unwrap();
    assert_eq!(ranked.iter().map(|r| r.index).collect::<Vec<_>>(), [1, 2, 0]);
    assert_eq!(ranked[0].candidate, "how to get a refund");
    assert_eq!(ranked[0].relevance, 0.9);
    assert!(matches!(ranked[0].answer, Answer::Noul(_)));
    let request = &mock.requests()[0];
    assert_eq!(request.state().unwrap()["query"], json!("How do I get a refund?"));
    assert_eq!(request.state().unwrap()["candidate"], json!("about refunds"));
    assert_eq!(request.question("relevance").unwrap()["type"], json!("noul"));
    assert!(request.question("relevance").unwrap()["criteria"]["true"].is_string());

    // Graded relevance, top-k, and a failing candidate.
    mock.reset();
    mock.enqueue(MockResponse::answers().score_level("relevance", 2, 3))
        .enqueue(MockResponse::error(500, "down"))
        .enqueue(MockResponse::answers().score_level("relevance", 1, 3));
    let client = mock.client();
    let rerank =
        client.rerank("q", candidates.clone()).concurrency(1).graded("How relevant?", ["no", "somewhat", "yes"]).top(1);
    let (ranked, errors) = rerank.run_lenient().await;
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].index, 0);
    assert_eq!(ranked[0].relevance, 1.0);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, 1);
    assert_eq!(mock.requests()[0].question("relevance").unwrap()["criteria"], json!(["no", "somewhat", "yes"]));

    mock.reset();
    mock.enqueue(MockResponse::error(500, "down"));
    assert!(mock.client().rerank("q", vec!["x"]).run().await.is_err());
}

#[tokio::test]
async fn find_uses_one_request() {
    let mock = MockTransport::new();
    mock.enqueue(MockResponse::answers().noul("any", 0.8).choice("which", [("0", 0.1), ("1", 0.7), ("2", 0.2)]));
    let lines = vec!["intro", "you may cancel any time", "fees"];
    let found = mock.client().find("When can I cancel?", lines.clone()).run().await.unwrap();
    assert_eq!(found.best(0.5), Some(1));
    assert_eq!(found.best(0.9), None);
    assert_eq!(found.top(2), &[(1, 0.7), (2, 0.2)]);
    assert_eq!(found.present.noul, 0.8);
    assert_eq!(mock.request_count(), 1);
    let request = mock.last_request().unwrap();
    assert_eq!(request.state().unwrap()["items"][1], json!({"id": "1", "text": "you may cancel any time"}));
    assert_eq!(request.question("which").unwrap()["criteria"], json!({"0": null, "1": null, "2": null}));
    assert_eq!(request.question("any").unwrap()["type"], json!("noul"));

    assert!(mock.client().find("q", Vec::<String>::new()).run().await.is_err());
}

#[tokio::test]
async fn evaluate_runs_examples_and_reports() {
    use typesafeai_sdk_community::eval::Example;
    use typesafeai_sdk_community::typed::{ChoiceLabels as _, ScoreLevels as _};
    use typesafeai_sdk_community::{ChoiceLabels, ScoreLevels, TypedChoice, TypedScore};

    #[derive(Clone, Copy, Debug, PartialEq, Eq, ChoiceLabels)]
    enum Tone {
        Angry,
        Calm,
    }
    #[derive(Clone, Copy, Debug, PartialEq, Eq, ScoreLevels)]
    enum Urgency {
        Low,
        High,
    }
    #[derive(Debug, Questions)]
    struct Full {
        #[noul("Billing?")]
        billing: NoulAnswer,
        #[choice("Tone?")]
        tone: TypedChoice<Tone>,
        #[score("Urgency?")]
        urgency: TypedScore<Urgency>,
    }

    let mock = MockTransport::new();
    for (p, tone, urgency) in
        [(0.9, Tone::Angry, Urgency::High), (0.2, Tone::Calm, Urgency::Low), (0.7, Tone::Calm, Urgency::High)]
    {
        mock.enqueue(
            MockResponse::answers()
                .noul("billing", p)
                .choice_typed_with(
                    "tone",
                    [(tone, 0.8), (if tone == Tone::Angry { Tone::Calm } else { Tone::Angry }, 0.2)],
                )
                .score_typed("urgency", urgency)
                .usage(7, 1),
        );
    }
    mock.enqueue(MockResponse::error(500, "down"));

    // Labels are (billing, tone, urgency); the last example fails.
    let examples = vec![
        Example::new(json!({"m": 1}), (true, Tone::Angry, Urgency::High)),
        Example::new(json!({"m": 2}), (false, Tone::Calm, Urgency::Low)),
        Example::new(json!({"m": 3}), (true, Tone::Angry, Urgency::High)),
        Example::new(json!({"m": 4}), (false, Tone::Calm, Urgency::Low)),
    ];
    let run = mock.client().evaluate::<Full, _>(examples).concurrency(1).run().await;
    assert_eq!(run.results.len(), 3);
    assert_eq!(run.errors.len(), 1);
    assert_eq!(run.errors[0].0, 3);
    assert_eq!(run.usage.input_tokens, Some(21));

    // Project labels for each report type.
    let binary = typesafeai_sdk_community::eval::binary(run.results.iter().map(|(a, l)| (a.billing.noul, l.0)));
    assert_eq!(binary.n, 3);
    assert_eq!(binary.positives, 2);
    assert!(binary.auc > 0.99);

    let choice = typesafeai_sdk_community::eval::choice(
        run.results
            .iter()
            .map(|(a, l)| (a.tone.choice.label().to_string(), a.tone.confidence, l.1.label().to_string())),
    );
    assert!((choice.accuracy - 2.0 / 3.0).abs() < 1e-9);
    assert_eq!(choice.per_label["angry"].support, 2);

    let score = typesafeai_sdk_community::eval::score(
        run.results
            .iter()
            .map(|(a, l)| (a.urgency.score, a.urgency.most_likely.level(), a.urgency.confidence, l.2.level())),
    );
    assert_eq!(score.exact, 1.0);

    // The run-level helpers with simple labels.
    let mock = MockTransport::new();
    mock.enqueue(
        MockResponse::answers()
            .noul("billing", 0.9)
            .choice_typed("tone", Tone::Angry)
            .score_typed("urgency", Urgency::High),
    );
    mock.enqueue(
        MockResponse::answers()
            .noul("billing", 0.1)
            .choice_typed("tone", Tone::Calm)
            .score_typed("urgency", Urgency::Low),
    );
    let run = mock.client().evaluate::<Full, bool>([("a", true), ("b", false)]).run().await;
    let report = run.binary(|full| full.billing.noul);
    assert!((report.brier - 0.01).abs() < 1e-9);
    let run = mock.client().evaluate::<Full, Tone>([("a", Tone::Angry)]).run().await;
    assert_eq!(run.results.len(), 0); // queue exhausted: the request failed with a 599
    assert_eq!(run.errors.len(), 1);
}

#[tokio::test]
async fn cancelled_batch_drops_work_and_leaves_the_transport_usable() {
    let mock = MockTransport::new();
    mock.fallback(MockResponse::answers().noul("billing", 0.5));
    let (client, _) = slow_client(&mock, Duration::from_millis(50));
    let run = client.batch::<Triage>(["a", "b", "c", "d", "e", "f"]).concurrency(2);
    // Six items, two at a time, 50ms each cannot finish in 80ms: the future is dropped mid-run.
    assert!(tokio::time::timeout(Duration::from_millis(80), run.run()).await.is_err());
    // Nothing is stuck: the same client and transport serve a fresh request immediately.
    let outcome = client.batch::<Triage>(["z"]).run().await;
    assert_eq!(outcome.succeeded(), 1);
}
