#![cfg(feature = "blocking")]

mod support;

use std::time::Duration;

use serde_json::json;
use support::{MockServer, Reply, models_body, system_one_body};
use typesafeai_sdk_community::blocking::TypeSafeClient;
use typesafeai_sdk_community::{ApiErrorKind, Choice, Error, Noul, RetryPolicy};

fn client(server: &MockServer, policy: RetryPolicy) -> TypeSafeClient {
    TypeSafeClient::builder().api_key("sk-test").base_url(server.url()).retry(policy).build().unwrap()
}

#[test]
fn system_one_round_trip() {
    let server = MockServer::start();
    server.enqueue(Reply::json(200, system_one_body()).header("x-typesafe-request-id", "b1"));
    let response = client(&server, RetryPolicy::none())
        .system_one(json!({"message": "hello"}))
        .question("billing", Noul::new("Billing?"))
        .question("tone", Choice::new("Tone?").labels(["angry", "calm", "excited"]))
        .model("jev-pinned")
        .header("x-call", "1")
        .send()
        .unwrap();
    assert_eq!(response.request_id(), Some("b1"));
    assert_eq!(response.choice("tone").unwrap().choice, "angry");

    let request = server.last_request();
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/systemone");
    assert_eq!(request.header("authorization"), Some("Bearer sk-test"));
    assert_eq!(request.header("x-call"), Some("1"));
    let body = request.json();
    assert_eq!(body["model"], json!("jev-pinned"));
    assert_eq!(body["state"], json!({"message": "hello"}));
    assert_eq!(body["questions"]["billing"], json!({"type": "noul", "instructions": "Billing?"}));
}

#[test]
fn send_as_send_raw_and_models() {
    #[derive(serde::Deserialize)]
    struct Model {
        model: String,
    }
    let server = MockServer::start();
    server.fallback(Reply::json(200, system_one_body()));
    let client = client(&server, RetryPolicy::none());
    let mine: Model = client.system_one("x").question("q", Noul::new("?")).send_as().unwrap();
    assert_eq!(mine.model, "jev-2026-09-15");
    let raw = client.system_one("x").question("q", Noul::new("?")).send_raw().unwrap();
    assert_eq!(raw.json::<serde_json::Value>().unwrap(), system_one_body());

    server.enqueue(Reply::json(200, models_body()));
    let models = client.models().list().timeout(Duration::from_secs(5)).send().unwrap();
    assert_eq!(models.models.len(), 2);
    assert_eq!(server.last_request().method, "GET");
}

#[test]
fn errors_and_retries() {
    let server = MockServer::start();
    server.enqueue(Reply::json(401, json!({"error": "bad key"})));
    let error = client(&server, RetryPolicy::none()).system_one("x").question("q", Noul::new("?")).send().unwrap_err();
    assert!(error.is_api_kind(ApiErrorKind::Authentication));
    assert_eq!(error.as_api().unwrap().message(), "bad key");

    let error = client(&server, RetryPolicy::none()).system_one("x").send().unwrap_err();
    assert!(matches!(error, Error::InvalidRequest(_)));

    let server = MockServer::start();
    server.enqueue(Reply::empty(503)).enqueue(Reply::drop()).enqueue(Reply::json(200, system_one_body()));
    let policy =
        RetryPolicy::default().backoff_initial(Duration::from_millis(5)).backoff_max(Duration::from_millis(10));
    client(&server, policy).system_one("x").question("q", Noul::new("?")).send().unwrap();
    assert_eq!(server.request_count(), 3);
    assert_eq!(server.last_request().header("x-typesafe-retry-count"), Some("2"));

    let server = MockServer::start();
    server.enqueue(Reply::json(200, system_one_body()).delay(Duration::from_millis(300)));
    let client = TypeSafeClient::builder()
        .api_key("sk-test")
        .base_url(server.url())
        .retry(RetryPolicy::none())
        .timeout(Duration::from_millis(50))
        .build()
        .unwrap();
    let error = client.system_one("x").question("q", Noul::new("?")).send().unwrap_err();
    assert!(matches!(error, Error::Timeout(_)), "{error}");
}
