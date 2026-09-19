mod support;

use std::time::Duration;

use serde::Deserialize;
use support::{MockServer, Reply, models_body, system_one_body};
use typesafe_sdk::{ApiErrorKind, Choice, Error, Noul, Question, RetryPolicy, Score, TypeSafeClient, VERSION, json};

fn client(server: &MockServer) -> TypeSafeClient {
    TypeSafeClient::builder()
        .api_key("sk-test")
        .base_url(format!("{}/", server.url()))
        .retry(RetryPolicy::none())
        .build()
        .expect("client builds")
}

#[tokio::test]
async fn system_one_parses_every_answer_type() {
    let server = MockServer::start();
    server.enqueue(Reply::json(200, system_one_body()).header("x-typesafe-request-id", "req_123"));

    let response = client(&server)
        .system_one("I was charged twice. Please help.")
        .question("billing", Noul::new("Is this about billing?"))
        .question("tone", Choice::new("Tone?").labels(["angry", "calm", "excited"]))
        .question("urgency", Score::new("Urgency?", ["Can wait", "This week", "Today"]))
        .send()
        .await
        .unwrap();

    assert_eq!(response.model, "jev-2026-09-15");
    assert_eq!(response.usage.input_tokens, Some(120));
    assert_eq!(response.usage.output_tokens, Some(12));
    assert_eq!(response.request_id(), Some("req_123"));
    assert_eq!(response.meta.status, 200);
    assert_eq!(response.answers.len(), 3);

    assert_eq!(response.noul("billing").unwrap().noul, 0.98);
    let tone = response.choice("tone").unwrap();
    assert_eq!(tone.choice, "angry");
    assert_eq!(tone.confidence, 0.9);
    assert_eq!(tone.probabilities["calm"], 0.1);
    let urgency = response.score("urgency").unwrap();
    assert_eq!(urgency.score, 1.7);
    assert_eq!(urgency.legend[&2], json!("Today"));
    assert_eq!(urgency.probabilities[&2], 0.8);
    assert_eq!(urgency.most_likely(), Some(2));

    assert_eq!(response.nouls().map(|(name, _)| name).collect::<Vec<_>>(), ["billing"]);
    assert_eq!(response.choices().count(), 1);
    assert_eq!(response.scores().count(), 1);
    assert!(response.noul("tone").is_none());
    assert!(response.answer("missing").is_none());
}

#[tokio::test]
async fn request_builder_can_be_awaited_directly() {
    let server = MockServer::start();
    server.enqueue(Reply::json(200, system_one_body()));
    let response = client(&server).system_one("x").question("q", Noul::new("?")).await.unwrap();
    assert_eq!(response.answers.len(), 3);
    server.enqueue(Reply::json(200, models_body()));
    let models = client(&server).models().list().await.unwrap();
    assert_eq!(models.models.len(), 2);
}

#[tokio::test]
async fn request_wire_format() {
    let server = MockServer::start();
    server.fallback(Reply::json(200, system_one_body()));
    let client = TypeSafeClient::builder()
        .api_key("sk-test")
        .base_url(server.url())
        .model("jev-custom")
        .retry(RetryPolicy::none())
        .header("x-default", "1")
        .header("x-typesafe-retry-count", "99")
        .header("authorization", "Bearer overridden")
        .build()
        .unwrap();

    client
        .system_one(json!({"document": "hello"}))
        .question("billing", Noul::new("Billing?").when_true("Money").when_false("Other"))
        .question("tone", Choice::new("Tone?").option("angry", "Upset").label("calm"))
        .question("urgency", Score::new(json!({"task": "Urgency"}), ["low", "high"]))
        .question("raw", Question::Custom(json!({"type": "future", "x": 1}).as_object().unwrap().clone()))
        .header("x-call", "2")
        .header("x-default", "override")
        .header("accept", "text/plain")
        .extra_body("metadata", json!({"trace": "abc"}))
        .extra_body("model", "jev-override")
        .send()
        .await
        .unwrap();

    let request = server.last_request();
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/systemone");
    assert_eq!(request.header("authorization"), Some("Bearer sk-test"));
    assert_eq!(request.header("accept"), Some("application/json"));
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert_eq!(request.header("user-agent"), Some(format!("typesafe-sdk-rust-community/{VERSION}").as_str()));
    assert_eq!(request.header("x-typesafe-sdk"), Some(format!("typesafe-sdk-rust-community/{VERSION}").as_str()));
    assert!(request.header("x-typesafe-runtime").unwrap().starts_with("rust/"));
    assert_eq!(request.header("x-typesafe-retry-count"), None);
    assert_eq!(request.header("x-default"), Some("override"));
    assert_eq!(request.header("x-call"), Some("2"));

    assert_eq!(
        request.json(),
        json!({
            "state": {"document": "hello"},
            "model": "jev-override",
            "questions": {
                "billing": {"type": "noul", "instructions": "Billing?", "criteria": {"true": "Money", "false": "Other"}},
                "tone": {"type": "choice", "instructions": "Tone?", "criteria": {"angry": "Upset", "calm": null}},
                "urgency": {"type": "score", "instructions": {"task": "Urgency"}, "criteria": ["low", "high"]},
                "raw": {"type": "future", "x": 1}
            },
            "metadata": {"trace": "abc"}
        })
    );
}

#[tokio::test]
async fn default_model_and_per_call_model() {
    let server = MockServer::start();
    server.fallback(Reply::json(200, system_one_body()));
    let client = client(&server);
    assert!(client.default_model() == "jev-latest" || std::env::var("TYPESAFE_DEFAULT_MODEL").is_ok());
    client.system_one("x").question("q", Noul::new("?")).send().await.unwrap();
    assert_eq!(server.last_request().json()["model"], json!(client.default_model()));
    client.system_one("x").question("q", Noul::new("?")).model("jev-pinned").send().await.unwrap();
    assert_eq!(server.last_request().json()["model"], json!("jev-pinned"));
}

#[tokio::test]
async fn questions_are_validated_before_sending() {
    let server = MockServer::start();
    let client = client(&server);

    let error = client.system_one("x").send().await.unwrap_err();
    assert!(matches!(error, Error::InvalidRequest(_)));
    assert_eq!(error.to_string(), "At least one question is required.");

    let error = client
        .system_one("x")
        .question("urgency", Score::from_levels(Vec::<serde_json::Value>::new()))
        .send()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Score question \"urgency\" has no criteria"));
    assert_eq!(server.request_count(), 0);
}

#[tokio::test]
async fn invalid_headers_are_reported() {
    let server = MockServer::start();
    let error = TypeSafeClient::builder().api_key("k").header("bad header", "v").build().unwrap_err();
    assert!(matches!(error, Error::Config(_)), "{error}");
    assert!(error.to_string().starts_with("Invalid header name"));

    let error = client(&server)
        .system_one("x")
        .question("q", Noul::new("?"))
        .header("x", "bad\nvalue")
        .send()
        .await
        .unwrap_err();
    assert!(error.to_string().starts_with("Invalid header value"), "{error}");
}

#[tokio::test]
async fn missing_api_key_is_a_config_error() {
    if std::env::var("TYPESAFE_API_KEY").is_ok_and(|v| !v.trim().is_empty()) {
        return; // Cannot exercise the missing-key path while the environment provides one.
    }
    let error = TypeSafeClient::new().unwrap_err();
    assert!(matches!(error, Error::Config(_)));
    assert!(error.to_string().contains("TYPESAFE_API_KEY"));
}

#[tokio::test]
async fn invalid_timeouts_and_policies_are_config_errors() {
    let error = TypeSafeClient::builder().api_key("k").timeout(Duration::ZERO).build().unwrap_err();
    assert!(error.to_string().contains("timeout must be a positive"));
    let error =
        TypeSafeClient::builder().api_key("k").retry(RetryPolicy::default().backoff_jitter(2.0)).build().unwrap_err();
    assert!(error.to_string().contains("backoff_jitter"));

    let server = MockServer::start();
    let error = client(&server)
        .system_one("x")
        .question("q", Noul::new("?"))
        .retry(RetryPolicy::default().backoff_jitter(2.0))
        .send()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("backoff_jitter"));
    let error =
        client(&server).system_one("x").question("q", Noul::new("?")).timeout(Duration::ZERO).send().await.unwrap_err();
    assert!(error.to_string().contains("timeout must be a positive"));
}

#[tokio::test]
async fn api_errors_are_classified_by_status() {
    let server = MockServer::start();
    let client = client(&server);
    for (status, kind) in [
        (400, ApiErrorKind::BadRequest),
        (401, ApiErrorKind::Authentication),
        (403, ApiErrorKind::PermissionDenied),
        (404, ApiErrorKind::NotFound),
        (422, ApiErrorKind::UnprocessableEntity),
        (429, ApiErrorKind::RateLimit),
        (500, ApiErrorKind::InternalServer),
        (503, ApiErrorKind::InternalServer),
        (418, ApiErrorKind::Other),
    ] {
        server
            .enqueue(Reply::json(status, json!({"error": {"message": "nope"}})).header("x-typesafe-request-id", "r1"));
        let error = client.system_one("x").question("q", Noul::new("?")).send().await.unwrap_err();
        assert!(error.is_api_kind(kind), "{status}: {error}");
        let api = error.as_api().unwrap();
        assert_eq!(api.status().as_u16(), status);
        assert_eq!(api.message(), "nope");
        assert_eq!(api.request_id(), Some("r1"));
        assert_eq!(error.request_id(), Some("r1"));
        assert_eq!(error.status().unwrap().as_u16(), status);
        assert_eq!(api.body().as_json(), Some(&json!({"error": {"message": "nope"}})));
        assert_eq!(error.to_string(), format!("POST {}/v1/systemone: {status} nope (request_id=r1)", server.url()));
    }
}

#[tokio::test]
async fn validation_error_details_are_joined() {
    let server = MockServer::start();
    server.enqueue(Reply::json(
        422,
        json!({"detail": [
            {"loc": ["body", "questions", "urgency", "score", "criteria"], "msg": "List should have at least 1 item", "type": "too_short"},
            {"loc": ["body", "state"], "msg": "Field required", "type": "missing"}
        ]}),
    ));
    let error = client(&server).system_one("x").question("q", Noul::new("?")).send().await.unwrap_err();
    assert_eq!(
        error.as_api().unwrap().message(),
        "questions.urgency.score.criteria: List should have at least 1 item; state: Field required"
    );
}

#[tokio::test]
async fn rate_limit_exposes_retry_after() {
    let server = MockServer::start();
    server.enqueue(Reply::text(429, "slow down").header("retry-after-ms", "1500"));
    let error = client(&server).system_one("x").question("q", Noul::new("?")).send().await.unwrap_err();
    let api = error.as_api().unwrap();
    assert_eq!(api.kind(), ApiErrorKind::RateLimit);
    assert_eq!(api.retry_after(), Some(Duration::from_millis(1500)));
    assert_eq!(api.message(), "slow down");
    assert_eq!(api.body().as_text(), Some("slow down"));
}

#[tokio::test]
async fn empty_and_non_json_error_bodies() {
    let server = MockServer::start();
    server.enqueue(Reply::empty(502));
    let error = client(&server).system_one("x").question("q", Noul::new("?")).send().await.unwrap_err();
    assert_eq!(error.to_string(), format!("POST {}/v1/systemone: 502 status code (no body)", server.url()));

    server.enqueue(Reply::text(500, "<html>oops</html>"));
    let error = client(&server).system_one("x").question("q", Noul::new("?")).send().await.unwrap_err();
    assert_eq!(error.as_api().unwrap().message(), "<html>oops</html>");
}

#[tokio::test]
async fn response_validation_errors_name_the_field() {
    let server = MockServer::start();
    let client = client(&server);

    server.enqueue(
        Reply::json(200, json!({"model": "m", "usage": {}, "answers": {"tone": {"type": "choice", "choice": "a"}}}))
            .header("x-typesafe-request-id", "r9"),
    );
    let error = client.system_one("x").question("q", Noul::new("?")).send().await.unwrap_err();
    let validation = error.as_response_validation().expect("validation error");
    assert_eq!(validation.field_path(), "answers.tone.confidence");
    assert_eq!(validation.status().as_u16(), 200);
    assert_eq!(validation.request_id(), Some("r9"));
    assert_eq!(
        error.to_string(),
        format!(
            "POST {}/v1/systemone: 200 Invalid response data at \"answers.tone.confidence\". (request_id=r9)",
            server.url()
        )
    );

    server.enqueue(Reply::text(200, "not json"));
    let error = client.system_one("x").question("q", Noul::new("?")).send().await.unwrap_err();
    assert_eq!(error.as_response_validation().unwrap().field_path(), "");

    server.enqueue(Reply::json(200, json!({"models": [{"name": "a"}]})));
    let error = client.models().list().send().await.unwrap_err();
    assert_eq!(error.as_response_validation().unwrap().field_path(), "models[0].description");
}

#[tokio::test]
async fn unknown_answer_types_are_dropped() {
    let server = MockServer::start();
    let mut body = system_one_body();
    body["answers"]["future"] = json!({"type": "hologram", "shape": "cube"});
    server.enqueue(Reply::json(200, body));
    let response = client(&server).system_one("x").question("q", Noul::new("?")).send().await.unwrap();
    assert_eq!(response.answers.len(), 3);
    assert!(response.answer("future").is_none());
}

#[tokio::test]
async fn send_as_decodes_custom_types_and_send_raw_keeps_bytes() {
    #[derive(Debug, Deserialize)]
    struct Mine {
        model: String,
        answers: MyAnswers,
    }
    #[derive(Debug, Deserialize)]
    struct MyAnswers {
        billing: typesafe_sdk::NoulAnswer,
        urgency: typesafe_sdk::ScoreAnswer,
    }

    let server = MockServer::start();
    server.fallback(Reply::json(200, system_one_body()).header("x-typesafe-request-id", "raw1"));
    let client = client(&server);

    let mine: Mine = client.system_one("x").question("q", Noul::new("?")).send_as().await.unwrap();
    assert_eq!(mine.model, "jev-2026-09-15");
    assert_eq!(mine.answers.billing.noul, 0.98);
    assert_eq!(mine.answers.urgency.legend[&0], json!("Can wait"));

    let raw = client.system_one("x").question("q", Noul::new("?")).send_raw().await.unwrap();
    assert_eq!(raw.meta.status.as_u16(), 200);
    assert_eq!(raw.request_id(), Some("raw1"));
    assert_eq!(raw.json::<serde_json::Value>().unwrap(), system_one_body());
    assert!(raw.text().contains("hologram") || raw.text().contains("billing"));

    #[derive(Debug, Deserialize)]
    struct Wrong {
        #[allow(dead_code)]
        nope: String,
    }
    let error = client.system_one("x").question("q", Noul::new("?")).send_as::<Wrong>().await.unwrap_err();
    assert_eq!(error.as_response_validation().unwrap().field_path(), "nope");
}

#[tokio::test]
async fn models_list() {
    let server = MockServer::start();
    server.enqueue(Reply::json(200, models_body()).header("x-typesafe-request-id", "m1"));
    let response = client(&server).models().list().header("x-extra", "yes").send().await.unwrap();
    assert_eq!(response.models.len(), 2);
    assert_eq!(response.models[0].name, "jev-latest");
    assert_eq!(response.models[1].release_date, "2026-09-15");
    assert_eq!(response.request_id(), Some("m1"));

    let request = server.last_request();
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/v1/models");
    assert_eq!(request.header("authorization"), Some("Bearer sk-test"));
    assert_eq!(request.header("content-type"), None);
    assert_eq!(request.header("x-extra"), Some("yes"));
    assert!(request.body.is_empty());
}

#[tokio::test]
async fn timeouts_and_dropped_connections_are_distinct_errors() {
    let server = MockServer::start();
    server.enqueue(Reply::json(200, system_one_body()).delay(Duration::from_millis(500)));
    let client = TypeSafeClient::builder()
        .api_key("sk-test")
        .base_url(server.url())
        .retry(RetryPolicy::none())
        .timeout(Duration::from_millis(100))
        .build()
        .unwrap();
    let error = client.system_one("x").question("q", Noul::new("?")).send().await.unwrap_err();
    match error {
        Error::Timeout(timeout) => assert_eq!(timeout.timeout(), Duration::from_millis(100)),
        other => panic!("expected timeout, got {other}"),
    }
    assert_eq!(error.to_string(), "Request timed out (timeout=100ms).");

    server.enqueue(Reply::drop());
    let error = client.system_one("x").question("q", Noul::new("?")).send().await.unwrap_err();
    assert!(matches!(error, Error::Connection(_)), "{error}");
    assert!(error.to_string().starts_with("Connection error:"));

    let unreachable = TypeSafeClient::builder()
        .api_key("sk-test")
        .base_url("http://127.0.0.1:9")
        .retry(RetryPolicy::none())
        .build()
        .unwrap();
    let error = unreachable.models().list().send().await.unwrap_err();
    assert!(matches!(error, Error::Connection(_)), "{error}");
}

#[tokio::test]
async fn custom_http_client_and_clone_share_state() {
    let server = MockServer::start();
    server.fallback(Reply::json(200, models_body()));
    let http = reqwest::Client::builder().user_agent("ignored").build().unwrap();
    let client = TypeSafeClient::builder().api_key("sk-test").base_url(server.url()).http_client(http).build().unwrap();
    let clone = client.clone();
    clone.models().list().send().await.unwrap();
    assert_eq!(
        server.last_request().header("user-agent"),
        Some(format!("typesafe-sdk-rust-community/{VERSION}").as_str())
    );
    assert_eq!(clone.base_url(), server.url());
    assert!(format!("{client:?}").contains("TypeSafeClient"));
    assert!(!format!("{client:?}").contains("sk-test"));
}
