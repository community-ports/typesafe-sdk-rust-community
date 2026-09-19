#![cfg(all(feature = "derive", feature = "test-util"))]

use std::time::Duration;

use serde_json::json;
use typesafeai_sdk_community::composite::{Composite as _, Weights};
use typesafeai_sdk_community::testing::{Cassette, MockResponse, MockTransport, Recorder};
use typesafeai_sdk_community::transport::Transport;
use typesafeai_sdk_community::typed::{Questions as _, Route as _};
use typesafeai_sdk_community::{
    AnswerError, ApiErrorKind, ChoiceLabels, Composite, Error, Noul, NoulAnswer, Questions, RetryPolicy, Route,
    ScoreAnswer, ScoreLevels, TypeSafeClient, TypedChoice, TypedScore,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ChoiceLabels)]
enum Tone {
    Angry,
    Calm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ScoreLevels)]
enum Anger {
    #[score("Calm")]
    Calm,
    #[score("Irritated")]
    Irritated,
    #[score("Furious")]
    Furious,
}

#[derive(Debug, PartialEq, Route)]
#[route("What does the customer want?", name = "intent", mock)]
enum Intent {
    #[route(describe = "Wants money back")]
    Refund {
        #[noul("Is the full amount requested?")]
        full_amount: bool,
        #[score("How angry is the customer?")]
        anger: TypedScore<Anger>,
    },
    #[route(label = "cancel-sub", describe = "Wants to cancel")]
    Cancel {
        #[noul("Is this a threat rather than a decision?", name = "cancel_threat")]
        threat: Option<NoulAnswer>,
    },
    #[route(describe = "None of the above")]
    Other,
}

#[derive(Debug, Questions, Composite)]
#[questions(mock)]
struct SpamRisk {
    #[noul("Asks for credentials?")]
    #[weight(0.45)]
    credentials: NoulAnswer,
    #[noul("Spoofed sender?")]
    #[weight(0.30)]
    spoofed: Option<NoulAnswer>,
    #[score("Unexpected reward?", levels = ["expected", "surprising", "too good"])]
    #[weight(0.25, name = "reward_level")]
    reward: ScoreAnswer,
    #[choice("Tone?")]
    #[weight(0.10, label = Tone::Calm, invert)]
    tone: TypedChoice<Tone>,
    #[noul("Not weighted")]
    ignored: bool,
}

#[test]
fn route_questions_wire_format() {
    let questions = serde_json::to_value(Intent::questions()).unwrap();
    assert_eq!(
        questions["intent"],
        json!({
            "type": "choice",
            "instructions": "What does the customer want?",
            "criteria": {"refund": "Wants money back", "cancel-sub": "Wants to cancel", "other": "None of the above"}
        })
    );
    assert_eq!(questions["refund.full_amount"]["type"], json!("noul"));
    assert_eq!(questions["refund.anger"]["criteria"], json!(["Calm", "Irritated", "Furious"]));
    assert_eq!(questions["cancel_threat"]["type"], json!("noul"));
    assert_eq!(questions.as_object().unwrap().len(), 4);
    assert_eq!(Intent::ROUTE_NAME, "intent");
    assert_eq!(Intent::LABELS, &["refund", "cancel-sub", "other"]);
    assert_eq!(Intent::Other.label(), "other");
}

#[tokio::test]
async fn route_selects_and_fills_the_variant() {
    let mock = MockTransport::new();
    mock.enqueue(
        Intent::mock()
            .refund()
            .refund_full_amount(0.9)
            .refund_anger(Anger::Furious)
            .cancel_threat(0.1) // speculative branch answered too, ignored by the route
            .with(|r| r.request_id("route-1").model("jev-test")),
    );
    let client = mock.client();

    let routed = client.route::<Intent>("I want ALL my money back").send_full().await.unwrap();
    assert_eq!(routed.route.label(), "refund");
    assert_eq!(routed.choice.choice, "refund");
    assert_eq!(routed.margin(), 1.0);
    assert_eq!(routed.probability("other"), 0.0);
    assert_eq!(routed.response.request_id(), Some("route-1"));
    match &routed.route {
        Intent::Refund { full_amount, anger } => {
            assert!(*full_amount);
            assert_eq!(anger.most_likely, Anger::Furious);
        }
        other => panic!("unexpected route {other:?}"),
    }

    let request = mock.last_request().unwrap();
    assert_eq!(request.model(), Some("jev-latest"));
    assert_eq!(request.question("intent").unwrap()["type"], json!("choice"));
    assert!(request.question("refund.anger").is_some());

    // Unit variant, and an Option field left unanswered.
    mock.enqueue(Intent::mock().route_with([("refund", 0.2), ("cancel-sub", 0.5), ("other", 0.3)]));
    let routed = client.route::<Intent>("x").send_full().await.unwrap();
    assert_eq!(routed.route, Intent::Cancel { threat: None });
    assert!((routed.margin() - 0.2).abs() < 1e-9);

    mock.enqueue(Intent::mock().other());
    assert_eq!(client.route::<Intent>("x").await.unwrap(), Intent::Other);
}

#[tokio::test]
async fn route_errors() {
    let mock = MockTransport::new();
    let client = mock.client();

    mock.enqueue(MockResponse::answers().choice_label("intent", "hologram"));
    let error = client.route::<Intent>("x").send().await.unwrap_err();
    assert!(matches!(error, Error::Answer(AnswerError::UnknownLabel { ref label, .. }) if label == "hologram"));

    mock.enqueue(MockResponse::answers().choice_label("intent", "refund"));
    let error = client.route::<Intent>("x").send().await.unwrap_err();
    assert!(
        matches!(error, Error::Answer(AnswerError::Missing { ref name }) if name == "refund.full_amount"),
        "{error}"
    );

    mock.enqueue(MockResponse::answers().noul("billing", 1.0));
    let error = client.route::<Intent>("x").send().await.unwrap_err();
    assert!(matches!(error, Error::Answer(AnswerError::Missing { ref name }) if name == "intent"));
}

#[test]
fn composite_scoring() {
    let risk = SpamRisk {
        credentials: NoulAnswer { noul: 0.8 },
        spoofed: Some(NoulAnswer { noul: 0.5 }),
        reward: ScoreAnswer {
            score: 1.0,
            confidence: 1.0,
            legend: [(0, json!("a")), (2, json!("c"))].into(),
            probabilities: [(1, 1.0)].into(),
        },
        tone: TypedChoice {
            choice: Tone::Angry,
            confidence: 0.7,
            probabilities: vec![(Tone::Angry, 0.7), (Tone::Calm, 0.3)],
        },
        ignored: true,
    };
    let weights = SpamRisk::default_weights();
    assert_eq!(weights.get("credentials"), Some(0.45));
    assert_eq!(weights.get("reward_level"), Some(0.25));
    assert_eq!(weights.get("ignored"), None);

    // signals: credentials 0.8, spoofed 0.5, reward 1/2 = 0.5, tone = 1 - P(calm) = 0.7
    let total = 0.45 + 0.30 + 0.25 + 0.10;
    let expected = (0.8 * 0.45 + 0.5 * 0.30 + 0.5 * 0.25 + 0.7 * 0.10) / total;
    assert!((risk.composite() - expected).abs() < 1e-9, "{}", risk.composite());

    let parts = risk.breakdown();
    assert_eq!(parts.iter().map(|p| p.name).collect::<Vec<_>>(), ["credentials", "spoofed", "reward_level", "tone"]);
    assert!((parts[3].signal.unwrap() - 0.7).abs() < 1e-9);

    let tuned = risk.composite_with(&Weights::from([("credentials", 1.0)]));
    assert!((tuned - 0.8).abs() < 1e-9);

    let missing = SpamRisk { spoofed: None, ..risk };
    let expected = (0.8 * 0.45 + 0.5 * 0.25 + 0.7 * 0.10) / (0.45 + 0.25 + 0.10);
    assert!((missing.composite() - expected).abs() < 1e-9);
}

#[tokio::test]
async fn generated_fixture_builder_answers_every_field() {
    let mock = MockTransport::new();
    mock.enqueue(
        SpamRisk::mock().credentials(0.9).spoofed(0.1).reward(2).tone(Tone::Calm).ignored(0.0).with(|r| r.usage(50, 5)),
    );
    let (risk, response) = mock.client().ask::<SpamRisk>("x").send_full().await.unwrap().into_parts();
    assert_eq!(risk.credentials.noul, 0.9);
    assert_eq!(risk.reward.score, 2.0);
    assert_eq!(risk.reward.probabilities[&2], 1.0);
    assert_eq!(risk.tone.choice, Tone::Calm);
    assert!(!risk.ignored);
    assert_eq!(response.usage.input_tokens, Some(50));

    mock.enqueue(
        SpamRisk::mock()
            .credentials(0.2)
            .reward_with([(0, 0.5), (1, 0.5)])
            .tone_with([(Tone::Angry, 0.4), (Tone::Calm, 0.6)])
            .ignored(1.0),
    );
    let risk = mock.client().ask::<SpamRisk>("x").await.unwrap();
    assert!(risk.spoofed.is_none());
    assert_eq!(risk.reward.score, 0.5);
    assert_eq!(risk.tone.choice, Tone::Calm);
    assert_eq!(risk.tone.probability(Tone::Angry), 0.4);
}

#[tokio::test]
async fn mock_transport_drives_real_client_behavior() {
    let mock = MockTransport::new();
    mock.enqueue(MockResponse::rate_limited(Duration::from_millis(20)))
        .enqueue(MockResponse::disconnect())
        .enqueue(MockResponse::timeout())
        .enqueue(MockResponse::answers().noul("q", 0.5));
    let client = TypeSafeClient::builder()
        .api_key("k")
        .transport(mock.clone())
        .retry(
            RetryPolicy::default()
                .max_retries(3)
                .backoff_initial(Duration::from_millis(1))
                .backoff_max(Duration::from_millis(2)),
        )
        .build()
        .unwrap();
    let response = client.system_one("s").question("q", Noul::new("?")).await.unwrap();
    assert_eq!(response.noul("q").unwrap().noul, 0.5);
    let attempts: Vec<u32> = mock.requests().iter().map(|r| r.attempt).collect();
    assert_eq!(attempts, [0, 1, 2, 3]);
    assert_eq!(mock.requests()[0].header("authorization"), Some("Bearer k"));
    assert_eq!(mock.requests()[0].path(), "/v1/systemone");
    assert_eq!(mock.pending(), 0);

    mock.enqueue(MockResponse::error(401, "bad key"));
    let error = client.models().list().await.unwrap_err();
    assert!(error.is_api_kind(ApiErrorKind::Authentication));
    assert_eq!(error.as_api().unwrap().message(), "bad key");

    mock.enqueue(MockResponse::models(["jev-latest", "jev-2026-09-15"]));
    assert_eq!(client.models().list().await.unwrap().models[1].name, "jev-2026-09-15");

    // Nothing queued and no fallback: a clear 599.
    let error = client.models().list().await.unwrap_err();
    assert_eq!(error.status().unwrap().as_u16(), 599);
    assert!(error.to_string().contains("no scripted response"));

    mock.fallback(MockResponse::json(200, json!({"models": []})));
    assert!(client.models().list().await.unwrap().models.is_empty());
    mock.reset();
    assert_eq!(mock.request_count(), 0);
}

#[tokio::test]
async fn record_and_replay() {
    // "Real" upstream is itself a mock here; Recorder only needs a Transport.
    let upstream = MockTransport::new();
    upstream.enqueue(MockResponse::answers().noul("q", 0.42).request_id("rec-1"));
    upstream.enqueue(MockResponse::error(422, "bad question"));
    let recorder = Recorder::new(upstream.clone());
    let client =
        TypeSafeClient::builder().api_key("k").retry(RetryPolicy::none()).transport(recorder.clone()).build().unwrap();
    client.system_one("s").question("q", Noul::new("?")).await.unwrap();
    client.system_one("s").question("q", Noul::new("?")).await.unwrap_err();
    assert!(recorder.reqwest_client().is_none());

    let cassette = recorder.cassette();
    assert_eq!(cassette.exchanges.len(), 2);
    assert_eq!(cassette.exchanges[0].request.body.as_ref().unwrap()["state"], json!("s"));
    assert_eq!(cassette.exchanges[0].response.status, 200);
    assert_eq!(cassette.exchanges[1].response.json.as_ref().unwrap()["error"]["message"], json!("bad question"));

    let dir = std::env::temp_dir().join(format!("typesafeai-cassette-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("triage.json");
    cassette.save(&path).unwrap();
    let loaded = Cassette::load(&path).unwrap();
    std::fs::remove_dir_all(&dir).ok();

    let replay = MockTransport::replay(loaded);
    let client = replay.client();
    let response = client.system_one("s").question("q", Noul::new("?")).await.unwrap();
    assert_eq!(response.noul("q").unwrap().noul, 0.42);
    assert_eq!(response.request_id(), Some("rec-1"));
    let error = client.system_one("s").question("q", Noul::new("?")).await.unwrap_err();
    assert!(error.is_api_kind(ApiErrorKind::UnprocessableEntity));
}

#[cfg(feature = "blocking")]
#[test]
fn blocking_route_and_mock() {
    let mock = MockTransport::new();
    mock.enqueue(Intent::mock().other());
    let client = mock.blocking_client();
    assert_eq!(client.route::<Intent>("x").send().unwrap(), Intent::Other);
    assert_eq!(mock.request_count(), 1);
}

#[derive(Debug, Questions)]
#[allow(dead_code)]
struct PathBound {
    #[noul("Is `ticket.message` about billing?")]
    billing: NoulAnswer,
    #[noul("Is the `sender` known?")]
    known: NoulAnswer,
}

#[tokio::test]
async fn state_path_check() {
    use typesafeai_sdk_community::typed::Questions as _;

    let mock = MockTransport::new();
    mock.fallback(MockResponse::answers().noul("billing", 0.5).noul("known", 0.5));
    let client = mock.client(); // path checking on by default

    // Everything referenced exists: sends normally.
    let ok_state = json!({"ticket": {"message": "hi"}, "meta": {"sender": "a@b"}});
    client.ask::<PathBound>(ok_state.clone()).await.unwrap();
    assert_eq!(mock.request_count(), 1);

    // A renamed field: fails before sending, naming the question and the path.
    let bad_state = json!({"ticket": {"body": "hi"}, "meta": {"sender": "a@b"}});
    let error = client.ask::<PathBound>(bad_state.clone()).await.unwrap_err();
    let Error::StatePath(problem) = &error else { panic!("expected StatePath, got {error}") };
    assert_eq!(problem.issues.len(), 1);
    assert_eq!(problem.issues[0].question, "billing");
    assert_eq!(problem.issues[0].path, "ticket.message");
    assert_eq!(mock.request_count(), 1);
    assert_eq!(PathBound::check_paths(&bad_state).len(), 1);

    // Per-call opt out, and a client with the check off.
    client.ask::<PathBound>(bad_state.clone()).skip_path_check().await.unwrap();
    assert_eq!(mock.request_count(), 2);
    let unchecked =
        TypeSafeClient::builder().api_key("k").retry(RetryPolicy::none()).transport(mock.clone()).build().unwrap();
    unchecked.ask::<PathBound>(bad_state.clone()).await.unwrap();
    assert!(unchecked.ask::<PathBound>(bad_state.clone()).check_paths().await.is_err());

    // Batch: each item is checked against its own state.
    let outcome = client.batch::<PathBound>([ok_state, bad_state]).check_paths().run().await;
    assert_eq!(outcome.succeeded(), 1);
    assert!(matches!(outcome.results[1], Err(Error::StatePath(_))));
}

#[tokio::test]
async fn response_cache() {
    use typesafeai_sdk_community::cache::{Cache, CacheStats};

    let mock = MockTransport::new();
    mock.enqueue(MockResponse::answers().noul("billing", 0.9).request_id("first"))
        .enqueue(MockResponse::answers().noul("billing", 0.1).request_id("second"))
        .enqueue(MockResponse::error(500, "down"))
        .enqueue(MockResponse::answers().noul("billing", 0.5).request_id("third"))
        .enqueue(MockResponse::answers().noul("billing", 0.7).request_id("fourth"));
    let cache = Cache::in_memory(100).ttl(Duration::from_secs(60));
    let client = TypeSafeClient::builder()
        .api_key("k")
        .retry(RetryPolicy::none())
        .transport(mock.clone())
        .cache(cache.clone())
        .build()
        .unwrap();
    fn ask(client: &TypeSafeClient) -> typesafeai_sdk_community::SystemOneRequest<'_, TypeSafeClient> {
        client.system_one("same state").question("billing", Noul::new("Billing?"))
    }

    // Miss, then hit: same answer, same request ID, no second request.
    let first = ask(&client).await.unwrap();
    assert!(!first.meta.from_cache);
    let hit = ask(&client).await.unwrap();
    assert!(hit.meta.from_cache);
    assert_eq!(hit.noul("billing").unwrap().noul, 0.9);
    assert_eq!(hit.request_id(), Some("first"));
    assert_eq!(mock.request_count(), 1);
    assert_eq!(cache.stats(), CacheStats { hits: 1, misses: 1, stores: 1 });

    // A different scope is a different key.
    let scoped = ask(&client).cache_scope("tenant-b").await.unwrap();
    assert!(!scoped.meta.from_cache);
    assert_eq!(scoped.request_id(), Some("second"));
    assert_eq!(mock.request_count(), 2);

    // Errors are not cached: the next call for that scope goes out again.
    let error = ask(&client).cache_scope("tenant-c").await.unwrap_err();
    assert_eq!(error.status().unwrap().as_u16(), 500);
    let after = ask(&client).cache_scope("tenant-c").await.unwrap();
    assert_eq!(after.request_id(), Some("third"));
    assert_eq!(mock.request_count(), 4);

    // refresh() skips the read and replaces the entry; no_cache() touches nothing.
    let refreshed = ask(&client).refresh().await.unwrap();
    assert_eq!(refreshed.request_id(), Some("fourth"));
    assert_eq!(ask(&client).await.unwrap().request_id(), Some("fourth"));
    mock.enqueue(MockResponse::answers().noul("billing", 0.2).request_id("bypass"));
    assert_eq!(ask(&client).no_cache().await.unwrap().request_id(), Some("bypass"));
    assert_eq!(ask(&client).await.unwrap().request_id(), Some("fourth"));

    // Typed requests and routes go through the same cache.
    mock.enqueue(SpamRisk::mock().credentials(0.3).reward(0).tone(Tone::Calm).ignored(0.0));
    let risk = client.ask::<SpamRisk>("same state").send_full().await.unwrap();
    assert!(!risk.response.meta.from_cache);
    let again = client.ask::<SpamRisk>("same state").send_full().await.unwrap();
    assert!(again.response.meta.from_cache);
    assert_eq!(again.credentials.noul, 0.3);

    // Invalidate by key, and clear.
    let request_count = mock.request_count();
    cache.clear();
    mock.enqueue(MockResponse::answers().noul("billing", 0.4).request_id("after-clear"));
    assert_eq!(ask(&client).await.unwrap().request_id(), Some("after-clear"));
    assert_eq!(mock.request_count(), request_count + 1);

    // TTL expiry.
    let short = Cache::in_memory(10).ttl(Duration::from_millis(5));
    let client = TypeSafeClient::builder()
        .api_key("k")
        .retry(RetryPolicy::none())
        .transport(mock.clone())
        .cache(short)
        .build()
        .unwrap();
    mock.enqueue(MockResponse::answers().noul("billing", 0.6).request_id("ttl-1"));
    mock.enqueue(MockResponse::answers().noul("billing", 0.6).request_id("ttl-2"));
    assert_eq!(ask(&client).await.unwrap().request_id(), Some("ttl-1"));
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(ask(&client).await.unwrap().request_id(), Some("ttl-2"));

    // Custom key: everything for a scope shares one entry.
    let custom = Cache::in_memory(10).key(|input| format!("scope:{}", input.scope.unwrap_or("default")));
    let client = TypeSafeClient::builder()
        .api_key("k")
        .retry(RetryPolicy::none())
        .transport(mock.clone())
        .cache(custom)
        .build()
        .unwrap();
    mock.enqueue(MockResponse::answers().noul("billing", 0.8).request_id("custom"));
    ask(&client).await.unwrap();
    let other_state = client.system_one("different state").question("billing", Noul::new("Billing?")).await.unwrap();
    assert!(other_state.meta.from_cache);
    assert_eq!(other_state.request_id(), Some("custom"));

    // Models listing is never cached.
    mock.enqueue(MockResponse::models(["a"])).enqueue(MockResponse::models(["b"]));
    assert_eq!(client.models().list().await.unwrap().models[0].name, "a");
    assert_eq!(client.models().list().await.unwrap().models[0].name, "b");
}

#[cfg(feature = "blocking")]
#[test]
fn blocking_cache() {
    use typesafeai_sdk_community::cache::Cache;
    let mock = MockTransport::new();
    mock.enqueue(MockResponse::answers().noul("q", 0.5).request_id("b1"));
    let client = typesafeai_sdk_community::blocking::TypeSafeClient::builder()
        .api_key("k")
        .retry(RetryPolicy::none())
        .transport(mock.clone())
        .cache(Cache::in_memory(10))
        .build()
        .unwrap();
    client.system_one("s").question("q", Noul::new("?")).send().unwrap();
    let hit = client.system_one("s").question("q", Noul::new("?")).send().unwrap();
    assert!(hit.meta.from_cache);
    assert_eq!(mock.request_count(), 1);
    assert_eq!(client.cache().unwrap().stats().hits, 1);
}
