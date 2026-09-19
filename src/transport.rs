//! The HTTP layer: the [`Transport`] trait the clients send through, the default `reqwest`
//! transports, and (internally) request preparation, response decoding, and the retry loop.
//!
//! Most users never touch this module. Implement [`Transport`] to send through your own HTTP
//! stack or to observe requests; use the `testing` module (feature `test-util`) for scripted
//! responses without a network.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use reqwest::{Method, StatusCode, Url};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config::{Config, validate_timeout};
use crate::constants::{
    JSON_CONTENT_TYPE, REQUEST_ID_HEADER, RETRY_COUNT_HEADER, RUNTIME_HEADER, SDK_HEADER, runtime, sdk_identifier,
};
use crate::error::{ApiError, ConnectionError, Error, ResponseBody, ResponseValidationError, Result, TimeoutError};
use crate::logging::{TARGET, body_text, redacted};
use crate::response::{
    Answer, ChoiceAnswer, ListModelsResponse, NoulAnswer, RawResponse, ResponseMeta, ScoreAnswer, SystemOneResponse,
    Usage,
};
use crate::retry::RetryPolicy;

/// An immutable description of a single HTTP request to send.
#[derive(Clone, Debug)]
pub(crate) struct PreparedRequest {
    pub(crate) method: Method,
    pub(crate) url: String,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Option<Vec<u8>>,
    pub(crate) timeout: Duration,
}

impl PreparedRequest {
    /// `METHOD URL` without credentials, query parameters, or fragment, for error messages.
    pub(crate) fn endpoint(&self) -> String {
        let url = Url::parse(&self.url)
            .map(|mut url| {
                // These only fail for cannot-be-a-base URLs (`mailto:` and the like), which an
                // http(s) API root never is; the URL is used unchanged in that case.
                let _ = url.set_username("");
                let _ = url.set_password(None);
                url.set_query(None);
                url.set_fragment(None);
                url.to_string()
            })
            .unwrap_or_else(|_| self.url.clone());
        format!("{} {}", self.method, url)
    }

    /// Headers for one attempt: the prepared headers plus the retry count after the first try.
    pub(crate) fn attempt_headers(&self, attempts: u32) -> HeaderMap {
        let mut headers = self.headers.clone();
        if attempts > 0 {
            headers.insert(HeaderName::from_static(RETRY_COUNT_HEADER), HeaderValue::from(attempts));
            tracing::info!(target: TARGET, "{} {} retry {}", self.method, self.url, attempts);
        }
        tracing::debug!(
            target: TARGET,
            "{} {} -> headers={:?} body={}",
            self.method,
            self.url,
            redacted(&headers),
            body_text(self.body.as_deref())
        );
        headers
    }
}

impl HttpResponse {
    pub(crate) fn log_received(&self, request: &PreparedRequest, started: Instant) {
        tracing::info!(
            target: TARGET,
            "{} {} <- {} in {}ms (request {})",
            request.method,
            request.url,
            self.status.as_u16(),
            started.elapsed().as_millis(),
            self.headers.get(REQUEST_ID_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("-")
        );
        tracing::debug!(
            target: TARGET,
            "{} {} <- headers={:?} body={}",
            request.method,
            request.url,
            redacted(&self.headers),
            body_text(Some(&self.body))
        );
    }
}

/// Build a request: merge default and per-call headers, then apply the protected SDK headers.
pub(crate) fn prepare<B: Serialize>(
    config: &Config,
    method: Method,
    path: &str,
    body: Option<&B>,
    timeout: Option<Duration>,
    extra_headers: &HeaderMap,
) -> Result<PreparedRequest> {
    let mut headers = config.default_headers.clone();
    for (name, value) in extra_headers {
        headers.insert(name.clone(), value.clone());
    }
    headers.remove(RETRY_COUNT_HEADER);
    let bearer = HeaderValue::from_str(&format!("Bearer {}", config.api_key))
        .map_err(|_| Error::Config("The API key contains characters that are not valid in a header.".into()))?;
    headers.insert(AUTHORIZATION, bearer);
    headers.insert(ACCEPT, HeaderValue::from_static(JSON_CONTENT_TYPE));
    let identifier = HeaderValue::from_str(&sdk_identifier()).expect("SDK identifier is a valid header value");
    headers.insert(USER_AGENT, identifier.clone());
    headers.insert(HeaderName::from_static(SDK_HEADER), identifier);
    headers.insert(
        HeaderName::from_static(RUNTIME_HEADER),
        HeaderValue::from_str(&runtime()).unwrap_or_else(|_| HeaderValue::from_static("rust")),
    );
    let body = match body {
        Some(body) => {
            headers.insert(CONTENT_TYPE, HeaderValue::from_static(JSON_CONTENT_TYPE));
            Some(serde_json::to_vec(body).map_err(|error| {
                Error::InvalidRequest(format!("The request body could not be encoded as JSON: {error}"))
            })?)
        }
        None => None,
    };
    Ok(PreparedRequest {
        method,
        url: format!("{}{}", config.base_url, path),
        headers,
        body,
        timeout: validate_timeout(timeout.unwrap_or(config.timeout))?,
    })
}

pub(crate) fn map_reqwest_error(error: reqwest::Error, timeout: Duration) -> Error {
    if error.is_timeout() {
        Error::Timeout(TimeoutError::new(timeout))
    } else {
        Error::Connection(ConnectionError::from_reqwest(error))
    }
}

// --- Decoding ---------------------------------------------------------------------------------

/// A response type the transport can produce from a raw HTTP response.
pub(crate) trait Decode: Sized {
    fn decode(request: &PreparedRequest, raw: RawHttp) -> Result<Self>;

    /// Flag a value as served from the cache; response types record it in their `meta`.
    fn mark_cached(&mut self) {}
}

/// Any `serde` type decoded from the whole JSON body.
pub(crate) struct Custom<T>(pub(crate) T);

fn validation_error(request: &PreparedRequest, raw: &RawHttp, field_path: impl Into<String>) -> Error {
    ResponseValidationError::new(
        raw.status,
        ResponseBody::decode(&raw.body),
        raw.headers.clone(),
        field_path,
        Some(request.endpoint()),
    )
    .into()
}

/// Raise the matching [`ApiError`] for an unsuccessful status.
pub(crate) fn check_status(request: &PreparedRequest, raw: RawHttp) -> Result<RawHttp> {
    if raw.status.is_success() {
        Ok(raw)
    } else {
        Err(ApiError::new(raw.status, ResponseBody::decode(&raw.body), raw.headers, Some(request.endpoint())).into())
    }
}

/// Render a `serde_path_to_error` path as the SDK's dotted `field_path`, recovering the field
/// name of a `missing field` error from the message since serde reports it at the parent.
fn field_path(prefix: &str, error: &serde_path_to_error::Error<serde_json::Error>) -> String {
    fn push(path: &mut String, segment: &str) {
        if !path.is_empty() {
            path.push('.');
        }
        path.push_str(segment);
    }
    let mut path = prefix.to_string();
    for segment in error.path().iter() {
        match segment {
            serde_path_to_error::Segment::Map { key } => push(&mut path, key),
            serde_path_to_error::Segment::Seq { index } => path.push_str(&format!("[{index}]")),
            serde_path_to_error::Segment::Enum { .. } | serde_path_to_error::Segment::Unknown => {}
        }
    }
    let message = error.inner().to_string();
    if let Some(rest) = message.strip_prefix("missing field `")
        && let Some(end) = rest.find('`')
    {
        push(&mut path, &rest[..end]);
    }
    path
}

fn decode_value<T: DeserializeOwned>(prefix: &str, value: Value) -> std::result::Result<T, String> {
    serde_path_to_error::deserialize(value).map_err(|error| field_path(prefix, &error))
}

fn decode_bytes<T: DeserializeOwned>(bytes: &[u8]) -> std::result::Result<T, String> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    serde_path_to_error::deserialize(&mut deserializer).map_err(|error| {
        // A syntactically invalid body has no field to point at.
        if error.inner().is_syntax() || error.inner().is_eof() { String::new() } else { field_path("", &error) }
    })
}

impl Decode for RawResponse {
    fn decode(request: &PreparedRequest, raw: RawHttp) -> Result<Self> {
        let raw = check_status(request, raw)?;
        Ok(RawResponse { meta: ResponseMeta::new(raw.status, raw.headers), body: raw.body })
    }

    fn mark_cached(&mut self) {
        self.meta.from_cache = true;
    }
}

impl<T: DeserializeOwned> Decode for Custom<T> {
    fn decode(request: &PreparedRequest, raw: RawHttp) -> Result<Self> {
        let raw = check_status(request, raw)?;
        decode_bytes(&raw.body).map(Custom).map_err(|path| validation_error(request, &raw, path))
    }
}

impl Decode for ListModelsResponse {
    fn decode(request: &PreparedRequest, raw: RawHttp) -> Result<Self> {
        let raw = check_status(request, raw)?;
        let mut response: ListModelsResponse =
            decode_bytes(&raw.body).map_err(|path| validation_error(request, &raw, path))?;
        response.meta = ResponseMeta::new(raw.status, raw.headers);
        Ok(response)
    }

    fn mark_cached(&mut self) {
        self.meta.from_cache = true;
    }
}

impl Decode for SystemOneResponse {
    fn decode(request: &PreparedRequest, raw: RawHttp) -> Result<Self> {
        let raw = check_status(request, raw)?;
        let Ok(Value::Object(mut object)) = serde_json::from_slice::<Value>(&raw.body) else {
            return Err(validation_error(request, &raw, ""));
        };
        let mut answers = BTreeMap::new();
        match object.remove("answers") {
            None => {}
            Some(Value::Object(raw_answers)) => {
                for (name, mut raw_answer) in raw_answers {
                    let type_name = raw_answer.as_object_mut().and_then(|fields| fields.remove("type"));
                    let Some(Value::String(type_name)) = type_name else {
                        return Err(validation_error(request, &raw, format!("answers.{name}.type")));
                    };
                    let prefix = format!("answers.{name}");
                    let answer = match type_name.as_str() {
                        "noul" => decode_value::<NoulAnswer>(&prefix, raw_answer).map(Answer::Noul),
                        "choice" => decode_value::<ChoiceAnswer>(&prefix, raw_answer).map(Answer::Choice),
                        "score" => decode_value::<ScoreAnswer>(&prefix, raw_answer).map(Answer::Score),
                        other => {
                            // Forward-compat: ignore answer types this SDK version does not model.
                            // The raw payload is still available through `send_raw`.
                            tracing::warn!(target: TARGET, "Ignoring answer {name:?} with unrecognized type {other:?}");
                            continue;
                        }
                    }
                    .map_err(|path| validation_error(request, &raw, path))?;
                    answers.insert(name, answer);
                }
            }
            Some(_) => return Err(validation_error(request, &raw, "answers")),
        }
        #[derive(serde::Deserialize)]
        struct Top {
            model: String,
            usage: Usage,
        }
        let top: Top = decode_value("", Value::Object(object)).map_err(|path| validation_error(request, &raw, path))?;
        Ok(SystemOneResponse {
            model: top.model,
            usage: top.usage,
            answers,
            meta: ResponseMeta::new(raw.status, raw.headers),
        })
    }

    fn mark_cached(&mut self) {
        self.meta.from_cache = true;
    }
}

// --- Transport abstraction ----------------------------------------------------------------------

/// One HTTP attempt as handed to a [`Transport`]: the prepared request with this attempt's
/// headers already applied.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct HttpRequest {
    /// The HTTP method.
    pub method: Method,
    /// The absolute URL.
    pub url: String,
    /// All headers for this attempt, including authentication.
    pub headers: HeaderMap,
    /// The JSON body, if any.
    pub body: Option<Vec<u8>>,
    /// The timeout for the whole attempt.
    pub timeout: Duration,
}

/// The undecoded result of one HTTP attempt.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct HttpResponse {
    /// The HTTP status.
    pub status: StatusCode,
    /// The response headers.
    pub headers: HeaderMap,
    /// The raw body.
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// A response from its parts, for [`Transport`] implementations.
    pub fn new(status: StatusCode, headers: HeaderMap, body: Vec<u8>) -> Self {
        HttpResponse { status, headers, body }
    }
}

pub(crate) type RawHttp = HttpResponse;

/// Sends one HTTP attempt for the asynchronous client.
///
/// The SDK's default transport is `reqwest`. Implement this to route requests through your own
/// stack, record them, or answer them without a network (see the `testing` module under the
/// `test-util` feature). Retries, logging, and decoding stay in the SDK: a transport returns the
/// response it got, or an [`Error::Timeout`] / [`Error::Connection`] when it got none.
pub trait Transport: Send + Sync + 'static {
    /// Send one attempt.
    fn send(&self, request: HttpRequest) -> Pin<Box<dyn Future<Output = Result<HttpResponse>> + Send + '_>>;

    /// The `reqwest::Client` behind this transport, if it is the default one.
    fn reqwest_client(&self) -> Option<&reqwest::Client> {
        None
    }
}

/// Sends one HTTP attempt for the blocking client.
#[cfg(feature = "blocking")]
pub trait BlockingTransport: Send + Sync + 'static {
    /// Send one attempt.
    fn send(&self, request: HttpRequest) -> Result<HttpResponse>;

    /// The `reqwest::blocking::Client` behind this transport, if it is the default one.
    fn reqwest_client(&self) -> Option<&reqwest::blocking::Client> {
        None
    }
}

impl PreparedRequest {
    fn attempt(&self, attempts: u32) -> HttpRequest {
        HttpRequest {
            method: self.method.clone(),
            url: self.url.clone(),
            headers: self.attempt_headers(attempts),
            body: self.body.clone(),
            timeout: self.timeout,
        }
    }
}

/// The default transport: a `reqwest::Client`.
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    /// Wrap an existing client.
    pub fn new(client: reqwest::Client) -> Self {
        ReqwestTransport { client }
    }
}

impl Transport for ReqwestTransport {
    fn send(&self, request: HttpRequest) -> Pin<Box<dyn Future<Output = Result<HttpResponse>> + Send + '_>> {
        Box::pin(async move {
            let mut builder = self
                .client
                .request(request.method.clone(), &request.url)
                .headers(request.headers)
                .timeout(request.timeout);
            if let Some(body) = request.body {
                builder = builder.body(body);
            }
            let response = builder.send().await.map_err(|error| {
                tracing::info!(target: TARGET, "{} {} <- {}", request.method, request.url, error_kind(&error));
                map_reqwest_error(error, request.timeout)
            })?;
            let status = response.status();
            let headers = response.headers().clone();
            let body = response.bytes().await.map_err(|error| map_reqwest_error(error, request.timeout))?.to_vec();
            Ok(HttpResponse { status, headers, body })
        })
    }

    fn reqwest_client(&self) -> Option<&reqwest::Client> {
        Some(&self.client)
    }
}

/// The default blocking transport: a `reqwest::blocking::Client`.
#[cfg(feature = "blocking")]
pub struct ReqwestBlockingTransport {
    client: reqwest::blocking::Client,
}

#[cfg(feature = "blocking")]
impl ReqwestBlockingTransport {
    /// Wrap an existing client.
    pub fn new(client: reqwest::blocking::Client) -> Self {
        ReqwestBlockingTransport { client }
    }
}

#[cfg(feature = "blocking")]
impl BlockingTransport for ReqwestBlockingTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse> {
        let mut builder =
            self.client.request(request.method.clone(), &request.url).headers(request.headers).timeout(request.timeout);
        if let Some(body) = request.body {
            builder = builder.body(body);
        }
        let response = builder.send().map_err(|error| {
            tracing::info!(target: TARGET, "{} {} <- {}", request.method, request.url, error_kind(&error));
            map_reqwest_error(error, request.timeout)
        })?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.bytes().map_err(|error| map_reqwest_error(error, request.timeout))?.to_vec();
        Ok(HttpResponse { status, headers, body })
    }

    fn reqwest_client(&self) -> Option<&reqwest::blocking::Client> {
        Some(&self.client)
    }
}

pub(crate) fn error_kind(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "TimeoutException"
    } else if error.is_connect() {
        "ConnectError"
    } else if error.is_request() {
        "RequestError"
    } else if error.is_body() || error.is_decode() {
        "ReadError"
    } else {
        "TransportError"
    }
}

// --- Cache-aware decoding ---------------------------------------------------------------------

/// Per-call cache settings carried by a request.
#[derive(Clone, Debug, Default)]
pub(crate) struct CacheUse {
    pub(crate) mode: crate::cache::CacheMode,
    pub(crate) scope: Option<String>,
}

/// Try the cache before sending; returns the decoded hit, or the key to store under.
pub(crate) fn cache_lookup<T: Decode>(
    cache: Option<&crate::cache::Cache>,
    request: &PreparedRequest,
    usage: &CacheUse,
) -> Result<std::result::Result<T, Option<String>>> {
    use crate::cache::CacheMode;
    let Some(cache) = cache else { return Ok(Err(None)) };
    if usage.mode == CacheMode::Bypass || request.method != Method::POST {
        return Ok(Err(None));
    }
    let key = cache.request_key(request, usage.scope.as_deref());
    if usage.mode == CacheMode::Use
        && let Some(raw) = cache.lookup(&key)
    {
        let mut value = T::decode(request, raw)?;
        value.mark_cached();
        return Ok(Ok(value));
    }
    Ok(Err(Some(key)))
}

// --- Send loops ---------------------------------------------------------------------------------

/// Send with retries and return the successful (2xx) raw response; decoding is the caller's.
pub(crate) async fn send(
    transport: &dyn Transport,
    request: &PreparedRequest,
    policy: &RetryPolicy,
) -> Result<RawHttp> {
    let started = Instant::now();
    let mut attempts = 0u32;
    loop {
        let attempt_started = Instant::now();
        let result = match transport.send(request.attempt(attempts)).await {
            Ok(raw) => {
                raw.log_received(request, attempt_started);
                check_status(request, raw)
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(value) => return Ok(value),
            Err(error) => {
                attempts += 1;
                match policy.next_delay(attempts, started, &error) {
                    Some(delay) => tokio::time::sleep(delay).await,
                    None => return Err(error),
                }
            }
        }
    }
}

#[cfg(feature = "blocking")]
pub(crate) fn send_blocking(
    transport: &dyn BlockingTransport,
    request: &PreparedRequest,
    policy: &RetryPolicy,
) -> Result<RawHttp> {
    let started = Instant::now();
    let mut attempts = 0u32;
    loop {
        let attempt_started = Instant::now();
        let result = match transport.send(request.attempt(attempts)) {
            Ok(raw) => {
                raw.log_received(request, attempt_started);
                check_status(request, raw)
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(value) => return Ok(value),
            Err(error) => {
                attempts += 1;
                match policy.next_delay(attempts, started, &error) {
                    Some(delay) => std::thread::sleep(delay),
                    None => return Err(error),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request() -> PreparedRequest {
        PreparedRequest {
            method: Method::POST,
            url: "https://user:pw@api.typesafe.ai/v1/systemone?x=1#frag".into(),
            headers: HeaderMap::new(),
            body: None,
            timeout: Duration::from_secs(1),
        }
    }

    fn raw(body: Value) -> RawHttp {
        RawHttp { status: StatusCode::OK, headers: HeaderMap::new(), body: serde_json::to_vec(&body).unwrap() }
    }

    fn path_of(error: Error) -> String {
        error.as_response_validation().expect("validation error").field_path().to_string()
    }

    #[test]
    fn endpoint_strips_credentials_query_and_fragment() {
        assert_eq!(request().endpoint(), "POST https://api.typesafe.ai/v1/systemone");
    }

    #[test]
    fn system_one_field_paths() {
        let req = request();
        let decode = |body: Value| SystemOneResponse::decode(&req, raw(body));
        assert_eq!(path_of(decode(json!([1])).unwrap_err()), "");
        assert_eq!(path_of(decode(json!({"model": "m", "usage": {}, "answers": 1})).unwrap_err()), "answers");
        assert_eq!(
            path_of(decode(json!({"model": "m", "usage": {}, "answers": {"t": {"noul": 0.5}}})).unwrap_err()),
            "answers.t.type"
        );
        assert_eq!(
            path_of(
                decode(json!({"model": "m", "usage": {}, "answers": {"t": {"type": "noul", "noul": "x"}}}))
                    .unwrap_err()
            ),
            "answers.t.noul"
        );
        assert_eq!(
            path_of(
                decode(json!({"model": "m", "usage": {}, "answers": {"t": {"type": "choice", "choice": "a"}}}))
                    .unwrap_err()
            ),
            "answers.t.confidence"
        );
        assert_eq!(
            path_of(
                decode(json!({"model": "m", "usage": {}, "answers": {"t": {"type": "score", "score": 1.0, "confidence": 1.0, "legend": {"x": "a"}, "probabilities": {}}}}))
                    .unwrap_err()
            ),
            // serde reports an unparseable map key at the map itself.
            "answers.t.legend"
        );
        assert_eq!(path_of(decode(json!({"usage": {}})).unwrap_err()), "model");
        assert_eq!(path_of(decode(json!({"model": "m"})).unwrap_err()), "usage");
        assert_eq!(
            path_of(decode(json!({"model": "m", "usage": {"input_tokens": "1"}})).unwrap_err()),
            "usage.input_tokens"
        );
    }

    #[test]
    fn system_one_drops_unknown_answer_types_and_tolerates_missing_answers() {
        let req = request();
        let response = SystemOneResponse::decode(
            &req,
            raw(json!({"model": "m", "usage": {"input_tokens": 1, "output_tokens": 2}, "answers": {
                "a": {"type": "noul", "noul": 0.9},
                "b": {"type": "future", "x": 1}
            }})),
        )
        .unwrap();
        assert_eq!(response.answers.len(), 1);
        assert_eq!(response.noul("a").unwrap().noul, 0.9);
        assert_eq!(response.usage.input_tokens, Some(1));

        let response = SystemOneResponse::decode(&req, raw(json!({"model": "m", "usage": {}}))).unwrap();
        assert!(response.answers.is_empty());
        assert_eq!(response.usage, Usage::default());
    }

    #[test]
    fn list_models_field_paths() {
        let req = request();
        let error = ListModelsResponse::decode(
            &req,
            raw(json!({"models": [{"name": "a", "description": "d", "release_date": "r"}, {"name": 1}]})),
        )
        .unwrap_err();
        assert_eq!(path_of(error), "models[1].name");
        let error = ListModelsResponse::decode(&req, raw(json!({}))).unwrap_err();
        assert_eq!(path_of(error), "models");
        let invalid = RawHttp { status: StatusCode::OK, headers: HeaderMap::new(), body: b"nope".to_vec() };
        assert_eq!(path_of(ListModelsResponse::decode(&req, invalid).unwrap_err()), "");
    }

    #[test]
    fn non_success_becomes_api_error() {
        let req = request();
        let raw = RawHttp {
            status: StatusCode::NOT_FOUND,
            headers: HeaderMap::new(),
            body: b"{\"error\":\"nope\"}".to_vec(),
        };
        let error = RawResponse::decode(&req, raw).unwrap_err();
        let api = error.as_api().unwrap();
        assert_eq!(api.status(), StatusCode::NOT_FOUND);
        assert_eq!(api.message(), "nope");
        assert_eq!(api.endpoint(), Some("POST https://api.typesafe.ai/v1/systemone"));
    }
}
