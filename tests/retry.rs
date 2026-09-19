mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::json;
use support::{MockServer, Reply, system_one_body};
use typesafe_sdk::{Error, Noul, RetryPolicy, TypeSafeClient};

fn fast_policy() -> RetryPolicy {
    RetryPolicy::default()
        .backoff_initial(Duration::from_millis(10))
        .backoff_max(Duration::from_millis(20))
        .backoff_jitter(0.0)
}

fn client(server: &MockServer, policy: RetryPolicy) -> TypeSafeClient {
    TypeSafeClient::builder().api_key("sk-test").base_url(server.url()).retry(policy).build().unwrap()
}

fn retry_counts(server: &MockServer) -> Vec<Option<String>> {
    server.requests().iter().map(|r| r.header("x-typesafe-retry-count").map(str::to_string)).collect()
}

#[tokio::test]
async fn retries_5xx_then_succeeds_with_retry_count_header() {
    let server = MockServer::start();
    server.enqueue(Reply::empty(500)).enqueue(Reply::empty(503)).enqueue(Reply::json(200, system_one_body()));
    let response = client(&server, fast_policy()).system_one("x").question("q", Noul::new("?")).send().await.unwrap();
    assert_eq!(response.answers.len(), 3);
    assert_eq!(retry_counts(&server), [None, Some("1".into()), Some("2".into())]);
}

#[tokio::test]
async fn gives_up_after_max_retries() {
    let server = MockServer::start();
    server.fallback(Reply::text(500, "down"));
    let error = client(&server, fast_policy()).system_one("x").question("q", Noul::new("?")).send().await.unwrap_err();
    assert_eq!(error.status().unwrap().as_u16(), 500);
    assert_eq!(server.request_count(), 3);

    let server = MockServer::start();
    server.fallback(Reply::text(500, "down"));
    client(&server, fast_policy().max_retries(5))
        .system_one("x")
        .question("q", Noul::new("?"))
        .send()
        .await
        .unwrap_err();
    assert_eq!(server.request_count(), 6);
}

#[tokio::test]
async fn does_not_retry_client_errors_or_when_disabled() {
    let server = MockServer::start();
    server.fallback(Reply::text(400, "bad"));
    client(&server, fast_policy()).system_one("x").question("q", Noul::new("?")).send().await.unwrap_err();
    assert_eq!(server.request_count(), 1);

    let server = MockServer::start();
    server.fallback(Reply::text(500, "down"));
    client(&server, RetryPolicy::none()).system_one("x").question("q", Noul::new("?")).send().await.unwrap_err();
    assert_eq!(server.request_count(), 1);
}

#[tokio::test]
async fn custom_status_set_and_predicate() {
    let server = MockServer::start();
    server.fallback(Reply::text(418, "teapot"));
    client(&server, fast_policy().http_statuses([418]))
        .system_one("x")
        .question("q", Noul::new("?"))
        .send()
        .await
        .unwrap_err();
    assert_eq!(server.request_count(), 3);

    let server = MockServer::start();
    server.fallback(Reply::text(500, "down"));
    client(&server, fast_policy().http_statuses([]))
        .system_one("x")
        .question("q", Noul::new("?"))
        .send()
        .await
        .unwrap_err();
    assert_eq!(server.request_count(), 1);

    let server = MockServer::start();
    server.fallback(Reply::text(400, "bad"));
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&seen);
    let policy = fast_policy().max_retries(1).predicate(move |error| {
        counter.fetch_add(1, Ordering::SeqCst);
        matches!(error, Error::Api(api) if api.message() == "bad")
    });
    client(&server, policy).system_one("x").question("q", Noul::new("?")).send().await.unwrap_err();
    assert_eq!(server.request_count(), 2);
    assert_eq!(seen.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn honors_retry_after_headers() {
    let server = MockServer::start();
    server.enqueue(Reply::empty(429).header("retry-after-ms", "150")).enqueue(Reply::json(200, system_one_body()));
    let started = Instant::now();
    client(&server, fast_policy()).system_one("x").question("q", Noul::new("?")).send().await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(150), "{:?}", started.elapsed());
    assert_eq!(server.request_count(), 2);

    let server = MockServer::start();
    server.enqueue(Reply::empty(429).header("retry-after-ms", "500")).enqueue(Reply::json(200, system_one_body()));
    let started = Instant::now();
    client(&server, fast_policy().respect_retry_after(false))
        .system_one("x")
        .question("q", Noul::new("?"))
        .send()
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_millis(400), "{:?}", started.elapsed());
}

#[tokio::test]
async fn stops_before_exceeding_the_budget() {
    let server = MockServer::start();
    server.enqueue(Reply::empty(429).header("retry-after", "5")).enqueue(Reply::json(200, system_one_body()));
    let policy = fast_policy().timeout(Some(Duration::from_secs(1)));
    let started = Instant::now();
    let error = client(&server, policy).system_one("x").question("q", Noul::new("?")).send().await.unwrap_err();
    assert_eq!(error.status().unwrap().as_u16(), 429);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(server.request_count(), 1);

    // No budget: the same wait is honored.
    let server = MockServer::start();
    server.enqueue(Reply::empty(429).header("retry-after-ms", "100")).enqueue(Reply::json(200, system_one_body()));
    let policy = fast_policy().timeout(None);
    client(&server, policy).system_one("x").question("q", Noul::new("?")).send().await.unwrap();
    assert_eq!(server.request_count(), 2);
}

#[tokio::test]
async fn retries_connection_and_timeout_errors_when_enabled() {
    let server = MockServer::start();
    server.enqueue(Reply::drop()).enqueue(Reply::json(200, system_one_body()));
    client(&server, fast_policy()).system_one("x").question("q", Noul::new("?")).send().await.unwrap();
    assert_eq!(server.request_count(), 2);

    let server = MockServer::start();
    server.enqueue(Reply::drop()).enqueue(Reply::json(200, system_one_body()));
    let error = client(&server, fast_policy().api_connection_error(false))
        .system_one("x")
        .question("q", Noul::new("?"))
        .send()
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Connection(_)));
    assert_eq!(server.request_count(), 1);

    let server = MockServer::start();
    server
        .enqueue(Reply::json(200, system_one_body()).delay(Duration::from_millis(300)))
        .enqueue(Reply::json(200, system_one_body()));
    let client = TypeSafeClient::builder()
        .api_key("sk-test")
        .base_url(server.url())
        .retry(fast_policy())
        .timeout(Duration::from_millis(50))
        .build()
        .unwrap();
    client.system_one("x").question("q", Noul::new("?")).send().await.unwrap();
    assert_eq!(server.request_count(), 2);

    let server = MockServer::start();
    server
        .enqueue(Reply::json(200, system_one_body()).delay(Duration::from_millis(300)))
        .enqueue(Reply::json(200, system_one_body()));
    let client = TypeSafeClient::builder()
        .api_key("sk-test")
        .base_url(server.url())
        .retry(fast_policy().api_timeout_error(false))
        .timeout(Duration::from_millis(50))
        .build()
        .unwrap();
    let error = client.system_one("x").question("q", Noul::new("?")).send().await.unwrap_err();
    assert!(matches!(error, Error::Timeout(_)));
    assert_eq!(server.request_count(), 1);
}

#[tokio::test]
async fn per_call_policy_overrides_client_policy() {
    let server = MockServer::start();
    server.fallback(Reply::text(500, "down"));
    let client = client(&server, RetryPolicy::none());
    client.system_one("x").question("q", Noul::new("?")).retry(fast_policy().max_retries(1)).send().await.unwrap_err();
    assert_eq!(server.request_count(), 2);
    client.models().list().retry(fast_policy().max_retries(3)).send().await.unwrap_err();
    assert_eq!(server.request_count(), 6);
}

#[tokio::test]
async fn response_validation_errors_are_not_retried() {
    let server = MockServer::start();
    server.fallback(Reply::json(200, json!({"model": "m"})));
    let error = client(&server, fast_policy()).system_one("x").question("q", Noul::new("?")).send().await.unwrap_err();
    assert!(matches!(error, Error::ResponseValidation(_)));
    assert_eq!(server.request_count(), 1);
}
