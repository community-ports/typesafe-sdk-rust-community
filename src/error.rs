//! Error types and server message extraction.
//!
//! Every fallible SDK operation returns [`Result<T>`](crate::Result), whose error is the
//! [`Error`] enum. The variants correspond to the Python SDK's exception hierarchy:
//!
//! | Python | Rust |
//! | --- | --- |
//! | `TypeSafeError` (config / input problems) | [`Error::Config`], [`Error::InvalidRequest`] |
//! | `TypeSafeAPIError` and its status subclasses | [`Error::Api`] with [`ApiError::kind`] |
//! | `TypeSafeAPIResponseValidationError` | [`Error::ResponseValidation`] |
//! | `TypeSafeAPIConnectionError` | [`Error::Connection`] |
//! | `TypeSafeAPITimeoutError` | [`Error::Timeout`] |

use std::fmt;
use std::time::{Duration, SystemTime};

use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::Value;

use crate::constants::{MAX_ERROR_BODY_LENGTH, REQUEST_ID_HEADER, RETRY_AFTER_HEADER, RETRY_AFTER_MS_HEADER};

/// A specialized `Result` whose error type is [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// All errors produced by the SDK.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The client could not be configured: a missing API key, an invalid timeout, an invalid
    /// retry policy, or an invalid header.
    #[error("{0}")]
    Config(String),

    /// The request could not be built: no questions, an empty score rubric, or a body that
    /// could not be encoded as JSON.
    #[error("{0}")]
    InvalidRequest(String),

    /// The server returned an unsuccessful HTTP response after any retries.
    #[error(transparent)]
    Api(Box<ApiError>),

    /// A successful HTTP response whose body was missing or structurally invalid required data.
    #[error(transparent)]
    ResponseValidation(Box<ResponseValidationError>),

    /// A request failed without an HTTP response.
    #[error(transparent)]
    Connection(#[from] ConnectionError),

    /// A request exceeded its configured timeout.
    #[error(transparent)]
    Timeout(#[from] TimeoutError),

    /// A successful response could not be converted into the requested typed answers: a
    /// question was missing, had a different kind, or used a label or level the enum does not
    /// know.
    #[error(transparent)]
    Answer(#[from] AnswerError),
}

impl Error {
    /// The API error, if this is an [`Error::Api`].
    pub fn as_api(&self) -> Option<&ApiError> {
        match self {
            Error::Api(error) => Some(error),
            _ => None,
        }
    }

    /// The response validation error, if this is an [`Error::ResponseValidation`].
    pub fn as_response_validation(&self) -> Option<&ResponseValidationError> {
        match self {
            Error::ResponseValidation(error) => Some(error),
            _ => None,
        }
    }

    /// The HTTP status of the failed response, for [`Error::Api`] and
    /// [`Error::ResponseValidation`].
    pub fn status(&self) -> Option<StatusCode> {
        match self {
            Error::Api(error) => Some(error.status),
            Error::ResponseValidation(error) => Some(error.status),
            _ => None,
        }
    }

    /// The `x-typesafe-request-id` response header, when a response was received.
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Error::Api(error) => error.request_id(),
            Error::ResponseValidation(error) => error.request_id(),
            _ => None,
        }
    }

    /// Whether this error is an [`Error::Api`] of the given kind.
    pub fn is_api_kind(&self, kind: ApiErrorKind) -> bool {
        self.as_api().is_some_and(|error| error.kind() == kind)
    }
}

impl From<ApiError> for Error {
    fn from(error: ApiError) -> Self {
        Error::Api(Box::new(error))
    }
}

impl From<ResponseValidationError> for Error {
    fn from(error: ResponseValidationError) -> Self {
        Error::ResponseValidation(Box::new(error))
    }
}

/// A response answer could not be converted into a typed field.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AnswerError {
    /// The response had no answer under this question name.
    #[error("Answer {name:?} is missing from the response.")]
    Missing {
        /// The question name.
        name: String,
    },
    /// The answer is a different kind than the field expects.
    #[error("Answer {name:?} is a {actual} answer, but a {expected} answer was expected.")]
    WrongType {
        /// The question name.
        name: String,
        /// The kind the field expects.
        expected: &'static str,
        /// The kind the response contained.
        actual: &'static str,
    },
    /// A choice label in the response is not a variant of the target enum.
    #[error("Answer {name:?} uses label {label:?}, which the target enum does not define.")]
    UnknownLabel {
        /// The question name.
        name: String,
        /// The unrecognized label.
        label: String,
    },
    /// A score level in the response is not a variant of the target enum.
    #[error("Answer {name:?} uses level {level}, which the target enum does not define.")]
    UnknownLevel {
        /// The question name.
        name: String,
        /// The unrecognized level.
        level: u32,
    },
}

/// The decoded body of an HTTP response, kept for error reporting.
#[derive(Clone, Debug, PartialEq)]
pub enum ResponseBody {
    /// The body was empty.
    Empty,
    /// The body was valid JSON.
    Json(Value),
    /// The body was not JSON; its bytes are decoded as UTF-8 with replacement characters.
    Text(String),
}

impl ResponseBody {
    /// Lenient decoding: empty bytes, JSON, or replacement-decoded text.
    pub(crate) fn decode(bytes: &[u8]) -> Self {
        if bytes.is_empty() {
            return ResponseBody::Empty;
        }
        match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => ResponseBody::Json(value),
            Err(_) => ResponseBody::Text(String::from_utf8_lossy(bytes).into_owned()),
        }
    }

    /// The JSON value, if the body was JSON.
    pub fn as_json(&self) -> Option<&Value> {
        match self {
            ResponseBody::Json(value) => Some(value),
            _ => None,
        }
    }

    /// The text, if the body was not JSON.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ResponseBody::Text(text) => Some(text),
            _ => None,
        }
    }
}

/// The category of an [`ApiError`], derived from its HTTP status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ApiErrorKind {
    /// The request was invalid (400).
    BadRequest,
    /// Authentication failed (401).
    Authentication,
    /// Access was denied (403).
    PermissionDenied,
    /// The resource was not found (404).
    NotFound,
    /// The request failed server validation (422).
    UnprocessableEntity,
    /// The rate limit was exceeded (429).
    RateLimit,
    /// The server failed to process the request (5xx).
    InternalServer,
    /// Any other unsuccessful status.
    Other,
}

impl ApiErrorKind {
    /// Classify an HTTP status code.
    pub fn from_status(status: StatusCode) -> Self {
        match status.as_u16() {
            400 => ApiErrorKind::BadRequest,
            401 => ApiErrorKind::Authentication,
            403 => ApiErrorKind::PermissionDenied,
            404 => ApiErrorKind::NotFound,
            422 => ApiErrorKind::UnprocessableEntity,
            429 => ApiErrorKind::RateLimit,
            500..=599 => ApiErrorKind::InternalServer,
            _ => ApiErrorKind::Other,
        }
    }
}

/// An unsuccessful HTTP response with its body and request metadata.
#[derive(Clone)]
pub struct ApiError {
    status: StatusCode,
    body: ResponseBody,
    headers: HeaderMap,
    message: String,
    endpoint: Option<String>,
}

impl ApiError {
    /// Build an error from a response, deriving the message from the body.
    pub fn new(status: StatusCode, body: ResponseBody, headers: HeaderMap, endpoint: Option<String>) -> Self {
        let message = extract_message(&body).unwrap_or_else(|| match &body {
            ResponseBody::Empty => "status code (no body)".to_string(),
            ResponseBody::Text(text) => truncate(text),
            ResponseBody::Json(value) => truncate(&value.to_string()),
        });
        ApiError { status, body, headers, message, endpoint }
    }

    /// The category of this error.
    pub fn kind(&self) -> ApiErrorKind {
        ApiErrorKind::from_status(self.status)
    }

    /// HTTP response status code.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The server's JSON error body, plain response text, or empty.
    pub fn body(&self) -> &ResponseBody {
        &self.body
    }

    /// HTTP response headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// The message extracted from the server's error body, or a truncated rendering of it.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The request method and URL, without credentials, query parameters, or fragment.
    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }

    /// The `x-typesafe-request-id` response header, or `None` if absent.
    pub fn request_id(&self) -> Option<&str> {
        header_str(&self.headers, REQUEST_ID_HEADER)
    }

    /// The server's requested wait from `retry-after-ms` or `Retry-After`, or `None` if
    /// unavailable. Most useful for [`ApiErrorKind::RateLimit`] responses.
    pub fn retry_after(&self) -> Option<Duration> {
        parse_retry_after(&self.headers)
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(endpoint) = &self.endpoint {
            write!(f, "{endpoint}: ")?;
        }
        write!(f, "{}", self.status.as_u16())?;
        if !self.message.is_empty() {
            write!(f, " {}", self.message)?;
        }
        if let Some(request_id) = self.request_id() {
            write!(f, " (request_id={request_id})")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Body and headers are omitted, matching the Python SDK's repr.
        write!(f, "ApiError({:?})", self.to_string())
    }
}

impl std::error::Error for ApiError {}

/// A successful HTTP response whose body was missing or structurally invalid required data.
#[derive(Clone)]
pub struct ResponseValidationError {
    status: StatusCode,
    body: ResponseBody,
    headers: HeaderMap,
    field_path: String,
    endpoint: Option<String>,
}

impl ResponseValidationError {
    pub(crate) fn new(
        status: StatusCode,
        body: ResponseBody,
        headers: HeaderMap,
        field_path: impl Into<String>,
        endpoint: Option<String>,
    ) -> Self {
        ResponseValidationError { status, body, headers, field_path: field_path.into(), endpoint }
    }

    /// HTTP response status code.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The decoded response body.
    pub fn body(&self) -> &ResponseBody {
        &self.body
    }

    /// HTTP response headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// Dotted path to the offending field, such as `answers.tone.confidence`; empty when the
    /// body as a whole was not a JSON object.
    pub fn field_path(&self) -> &str {
        &self.field_path
    }

    /// The request method and URL, without credentials, query parameters, or fragment.
    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }

    /// The `x-typesafe-request-id` response header, or `None` if absent.
    pub fn request_id(&self) -> Option<&str> {
        header_str(&self.headers, REQUEST_ID_HEADER)
    }
}

impl fmt::Display for ResponseValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(endpoint) = &self.endpoint {
            write!(f, "{endpoint}: ")?;
        }
        write!(f, "{} Invalid response data at {:?}.", self.status.as_u16(), self.field_path)?;
        if let Some(request_id) = self.request_id() {
            write!(f, " (request_id={request_id})")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ResponseValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ResponseValidationError({:?})", self.to_string())
    }
}

impl std::error::Error for ResponseValidationError {}

/// A request failed without an HTTP response.
#[derive(Debug, thiserror::Error)]
#[error("Connection error: {message}")]
pub struct ConnectionError {
    message: String,
    #[source]
    source: Option<reqwest::Error>,
}

impl ConnectionError {
    pub(crate) fn from_reqwest(error: reqwest::Error) -> Self {
        // Strip the URL from the message so credentials embedded in a base URL are not echoed.
        let message = error.to_string();
        let message = match error.url() {
            Some(url) => message.replace(&format!(" for url ({url})"), ""),
            None => message,
        };
        ConnectionError { message, source: Some(error) }
    }

    /// The underlying transport error, when available.
    pub fn source_error(&self) -> Option<&reqwest::Error> {
        self.source.as_ref()
    }
}

/// A request exceeded its configured timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Request timed out (timeout={timeout:?}).")]
pub struct TimeoutError {
    timeout: Duration,
}

impl TimeoutError {
    pub(crate) fn new(timeout: Duration) -> Self {
        TimeoutError { timeout }
    }

    /// The timeout setting used for the request.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

// --- Helpers ----------------------------------------------------------------------------------

pub(crate) fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn truncate(raw: &str) -> String {
    if raw.chars().count() > MAX_ERROR_BODY_LENGTH {
        let mut out: String = raw.chars().take(MAX_ERROR_BODY_LENGTH).collect();
        out.push('…');
        out
    } else {
        raw.to_string()
    }
}

/// Parse `retry-after-ms` (milliseconds) or `Retry-After` (seconds or an HTTP date) into a
/// wait duration.
pub(crate) fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    for (name, multiplier) in [(RETRY_AFTER_MS_HEADER, 1.0), (RETRY_AFTER_HEADER, 1000.0)] {
        let Some(raw) = header_str(headers, name) else { continue };
        let trimmed = raw.trim();
        let parsed = if trimmed.is_empty() { Ok(0.0) } else { trimmed.parse::<f64>() };
        match parsed {
            Ok(value) if value.is_finite() => {
                if value >= 0.0 {
                    let delay_ms = value * multiplier;
                    if delay_ms.is_finite() {
                        return Some(Duration::from_secs_f64(delay_ms / 1000.0));
                    }
                } else if name == RETRY_AFTER_HEADER {
                    return None;
                }
            }
            Ok(_) => {}
            Err(_) => {
                if name == RETRY_AFTER_HEADER
                    && let Ok(date) = httpdate::parse_http_date(trimmed)
                {
                    return Some(date.duration_since(SystemTime::now()).unwrap_or(Duration::ZERO));
                }
            }
        }
    }
    None
}

/// Pull a human-readable message out of the server's error body, following the same
/// precedence as the Python SDK: `error`, `error.message`, `message`, `detail`,
/// `detail.message`, then FastAPI-style `detail[]` entries.
pub(crate) fn extract_message(body: &ResponseBody) -> Option<String> {
    let value = match body {
        ResponseBody::Empty => return None,
        ResponseBody::Text(text) => return if text.is_empty() { None } else { Some(text.clone()) },
        ResponseBody::Json(value) => value,
    };
    match value {
        Value::String(text) => return if text.is_empty() { None } else { Some(text.clone()) },
        Value::Object(_) => {}
        _ => return None,
    }
    let error = value.get("error");
    let message = value.get("message");
    let detail = value.get("detail");
    if let Some(Value::String(text)) = error {
        return Some(text.clone());
    }
    if let Some(Value::String(text)) = error.and_then(|e| e.get("message")) {
        return Some(text.clone());
    }
    if let Some(Value::String(text)) = message {
        return Some(text.clone());
    }
    match detail {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Object(object)) => object.get("message").and_then(Value::as_str).map(str::to_string),
        Some(Value::Array(entries)) => {
            let parts: Vec<String> = entries
                .iter()
                .filter_map(|entry| {
                    let msg = entry.get("msg")?.as_str()?;
                    let path = match entry.get("loc") {
                        Some(Value::Array(location)) => location
                            .iter()
                            .filter(|item| item.as_str() != Some("body"))
                            .map(|item| match item {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            })
                            .collect::<Vec<_>>()
                            .join("."),
                        _ => String::new(),
                    };
                    Some(if path.is_empty() { msg.to_string() } else { format!("{path}: {msg}") })
                })
                .collect();
            if parts.is_empty() { None } else { Some(parts.join("; ")) }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;
    use serde_json::json;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn retry_after_ms_takes_precedence() {
        let h = headers(&[("retry-after-ms", "250"), ("retry-after", "5")]);
        assert_eq!(parse_retry_after(&h), Some(Duration::from_millis(250)));
    }

    #[test]
    fn retry_after_seconds() {
        assert_eq!(parse_retry_after(&headers(&[("retry-after", "1.5")])), Some(Duration::from_millis(1500)));
        assert_eq!(parse_retry_after(&headers(&[("retry-after", "  ")])), Some(Duration::ZERO));
    }

    #[test]
    fn retry_after_negative_and_invalid() {
        assert_eq!(parse_retry_after(&headers(&[("retry-after", "-1")])), None);
        // A negative ms value falls through to the seconds header.
        assert_eq!(
            parse_retry_after(&headers(&[("retry-after-ms", "-1"), ("retry-after", "2")])),
            Some(Duration::from_secs(2))
        );
        assert_eq!(parse_retry_after(&headers(&[("retry-after", "soon")])), None);
        assert_eq!(parse_retry_after(&headers(&[("retry-after", "inf")])), None);
        assert_eq!(parse_retry_after(&headers(&[])), None);
    }

    #[test]
    fn retry_after_http_date_in_past_is_zero() {
        let h = headers(&[("retry-after", "Wed, 21 Oct 2015 07:28:00 GMT")]);
        assert_eq!(parse_retry_after(&h), Some(Duration::ZERO));
    }

    #[test]
    fn retry_after_http_date_in_future() {
        let future = SystemTime::now() + Duration::from_secs(120);
        let h = headers(&[("retry-after", &httpdate::fmt_http_date(future))]);
        let delay = parse_retry_after(&h).unwrap();
        assert!(delay > Duration::from_secs(100) && delay <= Duration::from_secs(120));
    }

    #[test]
    fn message_extraction_precedence() {
        let msg = |v: Value| extract_message(&ResponseBody::Json(v));
        assert_eq!(msg(json!({"error": "boom"})), Some("boom".into()));
        assert_eq!(msg(json!({"error": {"message": "nested"}})), Some("nested".into()));
        assert_eq!(msg(json!({"message": "plain"})), Some("plain".into()));
        assert_eq!(msg(json!({"detail": "text"})), Some("text".into()));
        assert_eq!(msg(json!({"detail": {"message": "obj"}})), Some("obj".into()));
        assert_eq!(
            msg(json!({"detail": [
                {"loc": ["body", "questions", "urgency"], "msg": "Field required"},
                {"msg": "bare"},
                {"loc": ["body", 0], "msg": "indexed"},
                "ignored"
            ]})),
            Some("questions.urgency: Field required; bare; 0: indexed".into())
        );
        assert_eq!(msg(json!({"detail": []})), None);
        assert_eq!(msg(json!({"other": 1})), None);
        assert_eq!(msg(json!("")), None);
        assert_eq!(msg(json!("str")), Some("str".into()));
        assert_eq!(msg(json!(42)), None);
        assert_eq!(extract_message(&ResponseBody::Text("plain text".into())), Some("plain text".into()));
        assert_eq!(extract_message(&ResponseBody::Empty), None);
    }

    #[test]
    fn api_error_display() {
        let h = headers(&[("x-typesafe-request-id", "req_1")]);
        let error = ApiError::new(
            StatusCode::BAD_REQUEST,
            ResponseBody::Json(json!({"error": "bad"})),
            h,
            Some("POST https://api.typesafe.ai/v1/systemone".into()),
        );
        assert_eq!(error.to_string(), "POST https://api.typesafe.ai/v1/systemone: 400 bad (request_id=req_1)");
        assert_eq!(format!("{error:?}"), format!("ApiError({:?})", error.to_string()));
        assert_eq!(error.kind(), ApiErrorKind::BadRequest);
    }

    #[test]
    fn api_error_fallback_messages() {
        let empty = ApiError::new(StatusCode::BAD_GATEWAY, ResponseBody::Empty, HeaderMap::new(), None);
        assert_eq!(empty.to_string(), "502 status code (no body)");
        assert_eq!(empty.kind(), ApiErrorKind::InternalServer);

        // Plain-text bodies are the message verbatim; only JSON without a message is truncated.
        let long = "x".repeat(300);
        let text = ApiError::new(StatusCode::IM_A_TEAPOT, ResponseBody::Text(long.clone()), HeaderMap::new(), None);
        assert_eq!(text.message(), long);
        assert_eq!(text.kind(), ApiErrorKind::Other);
        let long_json =
            ApiError::new(StatusCode::BAD_REQUEST, ResponseBody::Json(json!({"k": long})), HeaderMap::new(), None);
        assert_eq!(long_json.message().chars().count(), MAX_ERROR_BODY_LENGTH + 1);
        assert!(long_json.message().ends_with('…'));

        let json = ApiError::new(StatusCode::BAD_REQUEST, ResponseBody::Json(json!({"k": 1})), HeaderMap::new(), None);
        assert_eq!(json.message(), r#"{"k":1}"#);
    }

    #[test]
    fn error_kind_classification() {
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
            assert_eq!(ApiErrorKind::from_status(StatusCode::from_u16(status).unwrap()), kind, "{status}");
        }
    }

    #[test]
    fn response_body_decode() {
        assert_eq!(ResponseBody::decode(b""), ResponseBody::Empty);
        assert_eq!(ResponseBody::decode(b"{\"a\":1}"), ResponseBody::Json(json!({"a": 1})));
        assert_eq!(ResponseBody::decode(b"not json"), ResponseBody::Text("not json".into()));
        assert_eq!(ResponseBody::decode(&[0xff, b'x']), ResponseBody::Text("\u{fffd}x".into()));
    }
}
