//! Test code that uses the SDK without a network. Enable the `test-util` Cargo feature.
//!
//! [`MockTransport`] answers requests from a queue of scripted [`MockResponse`]s and records
//! every request it receives, so tests exercise real client code (retries, decoding, typed
//! parsing) against answers you choose:
//!
//! ```
//! use typesafeai_sdk_community::testing::{MockResponse, MockTransport};
//! use typesafeai_sdk_community::{ChoiceLabels, Noul};
//!
//! #[derive(Clone, Copy, Debug, PartialEq, Eq, ChoiceLabels)]
//! enum Tone { Angry, Calm }
//!
//! # #[tokio::main] async fn main() -> typesafeai_sdk_community::Result<()> {
//! let mock = MockTransport::new();
//! mock.enqueue(
//!     MockResponse::answers()
//!         .noul("billing", 0.92)
//!         .choice_typed("tone", Tone::Angry)
//!         .request_id("req_test_1"),
//! );
//! let client = mock.client();
//!
//! let response = client.system_one("I was charged twice").question("billing", Noul::new("Billing?")).await?;
//! assert_eq!(response.noul("billing").unwrap().noul, 0.92);
//! assert_eq!(response.choice_as::<Tone>("tone")?.choice, Tone::Angry);
//! assert_eq!(mock.requests()[0].model(), Some("jev-latest"));
//! # Ok(()) }
//! ```
//!
//! With `#[derive(Questions)]` and `#[questions(mock)]`, a question set also gets a typed
//! fixture builder (`Triage::mock().tone(Tone::Angry).build()`) that produces a
//! [`MockResponse`] with every field answered.
//!
//! [`Recorder`] wraps a real transport and captures exchanges into a [`Cassette`] that
//! [`MockTransport::replay`] can play back later, for tests that should track real API
//! behavior without calling it every run.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::client::TypeSafeClient;
use crate::error::{ConnectionError, Error, Result, TimeoutError};
use crate::response::{Answer, ChoiceAnswer, NoulAnswer, ScoreAnswer, Usage};
use crate::retry::RetryPolicy;
use crate::transport::{HttpRequest, HttpResponse, Transport};
use crate::typed::{ChoiceLabels, ScoreLevels};

// --- Scripted responses -------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Body {
    Answers { model: String, usage: Usage, answers: BTreeMap<String, Answer>, extra: Map<String, Value> },
    Json(Value),
    Bytes(Vec<u8>),
    Empty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Behavior {
    Respond,
    Timeout,
    Disconnect,
}

/// One scripted response for a [`MockTransport`].
#[derive(Clone, Debug)]
pub struct MockResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Body,
    behavior: Behavior,
}

impl MockResponse {
    fn with_body(status: u16, body: Body) -> Self {
        MockResponse { status, headers: Vec::new(), body, behavior: Behavior::Respond }
    }

    /// A successful System One response to fill with answers. Defaults to model `jev-latest`
    /// and zero usage.
    pub fn answers() -> Self {
        Self::with_body(
            200,
            Body::Answers {
                model: "jev-latest".to_string(),
                usage: Usage { input_tokens: Some(0), output_tokens: Some(0) },
                answers: BTreeMap::new(),
                extra: Map::new(),
            },
        )
    }

    /// A successful models listing with the given model names.
    pub fn models(names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let models: Vec<Value> = names
            .into_iter()
            .map(|name| {
                serde_json::json!({"name": name.into(), "description": "Mock model.", "release_date": "2026-01-01"})
            })
            .collect();
        Self::json(200, serde_json::json!({ "models": models }))
    }

    /// Any JSON body with a status.
    pub fn json(status: u16, body: Value) -> Self {
        Self::with_body(status, Body::Json(body)).header("content-type", "application/json")
    }

    /// A plain-text body with a status.
    pub fn text(status: u16, text: impl Into<String>) -> Self {
        Self::with_body(status, Body::Bytes(text.into().into_bytes())).header("content-type", "text/plain")
    }

    /// An empty body with a status.
    pub fn status(status: u16) -> Self {
        Self::with_body(status, Body::Empty)
    }

    /// An API error in the server's `{"error": {"message": ...}}` shape.
    pub fn error(status: u16, message: impl Into<String>) -> Self {
        Self::json(status, serde_json::json!({"error": {"message": message.into()}}))
    }

    /// A 429 with a `retry-after-ms` header.
    pub fn rate_limited(retry_after: Duration) -> Self {
        Self::error(429, "Rate limit exceeded").header("retry-after-ms", retry_after.as_millis().to_string())
    }

    /// The request times out instead of receiving a response.
    pub fn timeout() -> Self {
        MockResponse { behavior: Behavior::Timeout, ..Self::status(0) }
    }

    /// The connection drops without a response.
    pub fn disconnect() -> Self {
        MockResponse { behavior: Behavior::Disconnect, ..Self::status(0) }
    }

    /// Add a response header.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Set the `x-typesafe-request-id` header.
    pub fn request_id(self, id: impl Into<String>) -> Self {
        self.header("x-typesafe-request-id", id)
    }

    fn answers_mut(&mut self) -> &mut BTreeMap<String, Answer> {
        if !matches!(self.body, Body::Answers { .. }) {
            self.body = match Self::answers().body {
                body @ Body::Answers { .. } => body,
                _ => unreachable!(),
            };
        }
        match &mut self.body {
            Body::Answers { answers, .. } => answers,
            _ => unreachable!(),
        }
    }

    /// Add any answer under a question name.
    pub fn answer(mut self, name: impl Into<String>, answer: impl Into<Answer>) -> Self {
        self.answers_mut().insert(name.into(), answer.into());
        self
    }

    /// A yes/no answer with the given probability of yes.
    pub fn noul(self, name: impl Into<String>, probability: f64) -> Self {
        self.answer(name, NoulAnswer { noul: probability })
    }

    /// A choice answer from a full distribution; the label with the highest probability is
    /// selected and its probability is the confidence.
    pub fn choice(
        self,
        name: impl Into<String>,
        probabilities: impl IntoIterator<Item = (impl Into<String>, f64)>,
    ) -> Self {
        let probabilities: BTreeMap<String, f64> = probabilities.into_iter().map(|(l, p)| (l.into(), p)).collect();
        let (choice, confidence) =
            probabilities.iter().max_by(|a, b| a.1.total_cmp(b.1)).map(|(l, p)| (l.clone(), *p)).unwrap_or_default();
        self.answer(name, ChoiceAnswer { choice, confidence, probabilities })
    }

    /// A certain choice answer: the label gets probability 1.
    pub fn choice_label(self, name: impl Into<String>, label: impl Into<String>) -> Self {
        self.choice(name, [(label.into(), 1.0)])
    }

    /// A certain choice answer over a [`ChoiceLabels`] enum: every label is listed, the chosen
    /// one with probability 1.
    pub fn choice_typed<T: ChoiceLabels>(self, name: impl Into<String>, label: T) -> Self {
        self.choice(name, T::ALL.iter().map(|l| (l.label(), if *l == label { 1.0 } else { 0.0 })))
    }

    /// A choice answer over a [`ChoiceLabels`] enum from a distribution.
    pub fn choice_typed_with<T: ChoiceLabels>(
        self,
        name: impl Into<String>,
        probabilities: impl IntoIterator<Item = (T, f64)>,
    ) -> Self {
        self.choice(name, probabilities.into_iter().map(|(l, p)| (l.label(), p)))
    }

    /// A score answer from a distribution over levels, with an optional legend. The expected
    /// score is computed from the distribution and the confidence is the top probability.
    pub fn score(
        self,
        name: impl Into<String>,
        probabilities: impl IntoIterator<Item = (u32, f64)>,
        legend: impl IntoIterator<Item = (u32, impl Into<Value>)>,
    ) -> Self {
        let probabilities: BTreeMap<u32, f64> = probabilities.into_iter().collect();
        let score = probabilities.iter().map(|(l, p)| f64::from(*l) * p).sum();
        let confidence = probabilities.values().copied().fold(0.0, f64::max);
        let legend: BTreeMap<u32, Value> = legend.into_iter().map(|(l, d)| (l, d.into())).collect();
        self.answer(name, ScoreAnswer { score, confidence, legend, probabilities })
    }

    /// A certain score answer at one level of a rubric with `levels` entries.
    pub fn score_level(self, name: impl Into<String>, level: u32, levels: u32) -> Self {
        let probabilities = (0..levels.max(level + 1)).map(|l| (l, if l == level { 1.0 } else { 0.0 }));
        self.score(name, probabilities, std::iter::empty::<(u32, Value)>())
    }

    /// A certain score answer over a [`ScoreLevels`] enum, with the legend from the enum.
    pub fn score_typed<T: ScoreLevels>(self, name: impl Into<String>, level: T) -> Self {
        self.score(
            name,
            T::ALL.iter().map(|l| (l.level(), if *l == level { 1.0 } else { 0.0 })),
            T::ALL.iter().map(|l| (l.level(), l.describe())),
        )
    }

    /// A score answer over a [`ScoreLevels`] enum from a distribution.
    pub fn score_typed_with<T: ScoreLevels>(
        self,
        name: impl Into<String>,
        probabilities: impl IntoIterator<Item = (T, f64)>,
    ) -> Self {
        self.score(
            name,
            probabilities.into_iter().map(|(l, p)| (l.level(), p)),
            T::ALL.iter().map(|l| (l.level(), l.describe())),
        )
    }

    /// Set the model name reported in the response.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        if let Body::Answers { model: m, .. } = &mut self.body {
            *m = model.into();
        }
        self
    }

    /// Set the token usage reported in the response.
    pub fn usage(mut self, input_tokens: u64, output_tokens: u64) -> Self {
        if let Body::Answers { usage, .. } = &mut self.body {
            *usage = Usage { input_tokens: Some(input_tokens), output_tokens: Some(output_tokens) };
        }
        self
    }

    /// Add a top-level field to an answers body, for forward-compatibility tests.
    pub fn extra(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        if let Body::Answers { extra, .. } = &mut self.body {
            extra.insert(key.into(), value.into());
        }
        self
    }

    /// The JSON this response would send, for answers and JSON bodies.
    pub fn to_json(&self) -> Option<Value> {
        match &self.body {
            Body::Answers { model, usage, answers, extra } => {
                let mut object = Map::new();
                object.insert("model".into(), Value::String(model.clone()));
                object.insert("usage".into(), serde_json::to_value(usage).ok()?);
                object.insert("answers".into(), serde_json::to_value(answers).ok()?);
                for (key, value) in extra {
                    object.insert(key.clone(), value.clone());
                }
                Some(Value::Object(object))
            }
            Body::Json(value) => Some(value.clone()),
            Body::Bytes(_) | Body::Empty => None,
        }
    }

    fn into_http(self, request: &HttpRequest) -> Result<HttpResponse> {
        match self.behavior {
            Behavior::Timeout => return Err(Error::Timeout(TimeoutError::new(request.timeout))),
            Behavior::Disconnect => {
                return Err(Error::Connection(ConnectionError::mock("connection closed before message completed")));
            }
            Behavior::Respond => {}
        }
        let mut headers = HeaderMap::new();
        let is_json = matches!(self.body, Body::Answers { .. });
        let body = match &self.body {
            Body::Answers { .. } | Body::Json(_) => serde_json::to_vec(&self.to_json().unwrap_or(Value::Null))
                .map_err(|error| Error::InvalidRequest(format!("mock response is not serializable: {error}")))?,
            Body::Bytes(bytes) => bytes.clone(),
            Body::Empty => Vec::new(),
        };
        if is_json {
            headers.insert("content-type", HeaderValue::from_static("application/json"));
        }
        for (name, value) in &self.headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|error| Error::InvalidRequest(format!("mock header name {name:?}: {error}")))?;
            let value = HeaderValue::from_str(value)
                .map_err(|error| Error::InvalidRequest(format!("mock header value {value:?}: {error}")))?;
            headers.append(name, value);
        }
        let status = StatusCode::from_u16(self.status)
            .map_err(|error| Error::InvalidRequest(format!("mock status {}: {error}", self.status)))?;
        Ok(HttpResponse { status, headers, body })
    }
}

// --- Recorded requests ----------------------------------------------------------------------------

/// A request a [`MockTransport`] received.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct RecordedRequest {
    /// The HTTP method.
    pub method: String,
    /// The absolute URL.
    pub url: String,
    /// The headers, including authentication.
    pub headers: HeaderMap,
    /// The body decoded as JSON, when there was one and it parsed.
    pub body: Option<Value>,
    /// The retry count header, or 0 for a first attempt.
    pub attempt: u32,
}

impl RecordedRequest {
    /// The first value of a header.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    /// The URL path, e.g. `/v1/systemone`.
    pub fn path(&self) -> &str {
        let without_scheme = self.url.split_once("://").map_or(self.url.as_str(), |(_, rest)| rest);
        without_scheme.find('/').map_or("", |index| &without_scheme[index..])
    }

    /// The `state` sent in a System One request.
    pub fn state(&self) -> Option<&Value> {
        self.body.as_ref()?.get("state")
    }

    /// The `model` sent in a System One request.
    pub fn model(&self) -> Option<&str> {
        self.body.as_ref()?.get("model")?.as_str()
    }

    /// The `questions` sent in a System One request, keyed by name.
    pub fn questions(&self) -> Option<&Map<String, Value>> {
        self.body.as_ref()?.get("questions")?.as_object()
    }

    /// One question by name.
    pub fn question(&self, name: &str) -> Option<&Value> {
        self.questions()?.get(name)
    }
}

// --- Mock transport -------------------------------------------------------------------------------

#[derive(Default)]
struct State {
    queue: VecDeque<MockResponse>,
    fallback: Option<MockResponse>,
    requests: Vec<RecordedRequest>,
}

/// A [`Transport`] that answers from scripted responses and records what it receives.
///
/// Clone it freely: clones share the queue and the recorded requests, so a test can keep one
/// handle for assertions while the client owns another.
#[derive(Clone, Default)]
pub struct MockTransport {
    state: Arc<Mutex<State>>,
}

impl MockTransport {
    /// An empty mock. Requests fail with a 599 until responses are queued or a fallback is set.
    pub fn new() -> Self {
        Self::default()
    }

    /// A mock that plays a [`Cassette`]'s responses back in order.
    pub fn replay(cassette: Cassette) -> Self {
        let mock = Self::new();
        for exchange in cassette.exchanges {
            mock.enqueue(exchange.response.into_mock());
        }
        mock
    }

    /// Queue the next response. Responses are served in order. Accepts a [`MockResponse`] or a
    /// generated fixture builder.
    pub fn enqueue(&self, response: impl Into<MockResponse>) -> &Self {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).queue.push_back(response.into());
        self
    }

    /// The response served whenever the queue is empty.
    pub fn fallback(&self, response: impl Into<MockResponse>) -> &Self {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).fallback = Some(response.into());
        self
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).requests.clone()
    }

    /// The most recent request.
    pub fn last_request(&self) -> Option<RecordedRequest> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).requests.last().cloned()
    }

    /// How many requests were received.
    pub fn request_count(&self) -> usize {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).requests.len()
    }

    /// How many queued responses have not been served.
    pub fn pending(&self) -> usize {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).queue.len()
    }

    /// Forget recorded requests and queued responses.
    pub fn reset(&self) {
        *self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = State::default();
    }

    /// An async client wired to this mock, with a dummy API key, retries disabled, and state
    /// path checking on (so a test catches a question that references a missing state field).
    /// Build your own with [`TypeSafeClient::builder`] and
    /// [`transport`](crate::ClientBuilder::transport) to test retry behavior or skip the check.
    pub fn client(&self) -> TypeSafeClient {
        TypeSafeClient::builder()
            .api_key("mock-api-key")
            .base_url("http://mock.typesafe.invalid")
            .retry(RetryPolicy::none())
            .check_paths(true)
            .transport(self.clone())
            .build()
            .expect("mock client configuration is valid")
    }

    /// A blocking client wired to this mock; see [`client`](Self::client).
    #[cfg(feature = "blocking")]
    pub fn blocking_client(&self) -> crate::blocking::TypeSafeClient {
        crate::blocking::TypeSafeClient::builder()
            .api_key("mock-api-key")
            .base_url("http://mock.typesafe.invalid")
            .retry(RetryPolicy::none())
            .check_paths(true)
            .transport(self.clone())
            .build()
            .expect("mock client configuration is valid")
    }

    fn handle(&self, request: HttpRequest) -> Result<HttpResponse> {
        let response = {
            let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let attempt = request
                .headers
                .get("x-typesafe-retry-count")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            state.requests.push(RecordedRequest {
                method: request.method.to_string(),
                url: request.url.clone(),
                headers: request.headers.clone(),
                body: request.body.as_deref().and_then(|bytes| serde_json::from_slice(bytes).ok()),
                attempt,
            });
            state.queue.pop_front().or_else(|| state.fallback.clone())
        };
        response
            .unwrap_or_else(|| MockResponse::text(599, "MockTransport: no scripted response left for this request"))
            .into_http(&request)
    }
}

impl std::fmt::Debug for MockTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        f.debug_struct("MockTransport")
            .field("pending", &state.queue.len())
            .field("requests", &state.requests.len())
            .finish()
    }
}

impl Transport for MockTransport {
    fn send(&self, request: HttpRequest) -> Pin<Box<dyn Future<Output = Result<HttpResponse>> + Send + '_>> {
        let result = self.handle(request);
        Box::pin(async move { result })
    }
}

#[cfg(feature = "blocking")]
impl crate::transport::BlockingTransport for MockTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse> {
        self.handle(request)
    }
}

// --- Record and replay ------------------------------------------------------------------------------

/// A recorded request, without headers so the API key never lands on disk.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CassetteRequest {
    /// The HTTP method.
    pub method: String,
    /// The absolute URL.
    pub url: String,
    /// The JSON body, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

/// A recorded response.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CassetteResponse {
    /// The HTTP status.
    pub status: u16,
    /// Response headers, excluding `set-cookie`.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// The body as JSON when it parsed as JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
    /// The body as text when it was not JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl CassetteResponse {
    fn from_http(response: &HttpResponse) -> Self {
        let json: Option<Value> = serde_json::from_slice(&response.body).ok();
        let text = if json.is_none() && !response.body.is_empty() {
            Some(String::from_utf8_lossy(&response.body).into_owned())
        } else {
            None
        };
        CassetteResponse {
            status: response.status.as_u16(),
            headers: response
                .headers
                .iter()
                .filter(|(name, _)| name.as_str() != "set-cookie")
                .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_string())))
                .collect(),
            json,
            text,
        }
    }

    fn into_mock(self) -> MockResponse {
        let mut mock = match (self.json, self.text) {
            (Some(json), _) => MockResponse::json(self.status, json),
            (None, Some(text)) => MockResponse::text(self.status, text),
            (None, None) => MockResponse::status(self.status),
        };
        for (name, value) in self.headers {
            if name != "content-type" && name != "content-length" {
                mock = mock.header(name, value);
            }
        }
        mock
    }
}

/// One recorded request/response pair.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Exchange {
    /// What was sent.
    pub request: CassetteRequest,
    /// What came back.
    pub response: CassetteResponse,
}

/// Recorded exchanges, serializable to JSON for checking into a repository.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Cassette {
    /// The exchanges in the order they happened.
    pub exchanges: Vec<Exchange>,
}

impl Cassette {
    /// Read a cassette from a JSON file.
    pub fn load(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text).map_err(std::io::Error::other)
    }

    /// Write the cassette as pretty JSON.
    pub fn save(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }
}

/// A [`Transport`] that forwards to another transport and records every exchange.
///
/// ```no_run
/// use typesafeai_sdk_community::TypeSafeClient;
/// use typesafeai_sdk_community::testing::Recorder;
/// use typesafeai_sdk_community::transport::ReqwestTransport;
///
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let recorder = Recorder::new(ReqwestTransport::new(reqwest::Client::new()));
/// let client = TypeSafeClient::builder().transport(recorder.clone()).build()?;
/// // ... make requests ...
/// recorder.cassette().save("tests/fixtures/triage.json")?;
/// # Ok(()) }
/// ```
#[derive(Clone)]
pub struct Recorder {
    inner: Arc<dyn Transport>,
    exchanges: Arc<Mutex<Vec<Exchange>>>,
}

impl Recorder {
    /// Record everything sent through `inner`.
    pub fn new(inner: impl Transport) -> Self {
        Recorder { inner: Arc::new(inner), exchanges: Arc::new(Mutex::new(Vec::new())) }
    }

    /// The exchanges recorded so far.
    pub fn cassette(&self) -> Cassette {
        Cassette { exchanges: self.exchanges.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone() }
    }
}

impl Transport for Recorder {
    fn send(&self, request: HttpRequest) -> Pin<Box<dyn Future<Output = Result<HttpResponse>> + Send + '_>> {
        Box::pin(async move {
            let recorded = CassetteRequest {
                method: request.method.to_string(),
                url: request.url.clone(),
                body: request.body.as_deref().and_then(|bytes| serde_json::from_slice(bytes).ok()),
            };
            let response = self.inner.send(request).await?;
            self.exchanges
                .lock()
                .expect("recorder lock")
                .push(Exchange { request: recorded, response: CassetteResponse::from_http(&response) });
            Ok(response)
        })
    }

    fn reqwest_client(&self) -> Option<&reqwest::Client> {
        self.inner.reqwest_client()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn answers_body_shape() {
        let response = MockResponse::answers()
            .noul("billing", 0.9)
            .choice("tone", [("angry", 0.7), ("calm", 0.3)])
            .score("urgency", [(0, 0.2), (1, 0.8)], [(0, "low"), (1, "high")])
            .model("jev-test")
            .usage(10, 2)
            .extra("future", 1);
        assert_eq!(
            response.to_json().unwrap(),
            json!({
                "model": "jev-test",
                "usage": {"input_tokens": 10, "output_tokens": 2},
                "answers": {
                    "billing": {"type": "noul", "noul": 0.9},
                    "tone": {"type": "choice", "choice": "angry", "confidence": 0.7, "probabilities": {"angry": 0.7, "calm": 0.3}},
                    "urgency": {"type": "score", "score": 0.8, "confidence": 0.8, "legend": {"0": "low", "1": "high"}, "probabilities": {"0": 0.2, "1": 0.8}}
                },
                "future": 1
            })
        );
    }

    #[test]
    fn score_level_fills_levels() {
        let json = MockResponse::answers().score_level("u", 1, 3).to_json().unwrap();
        assert_eq!(json["answers"]["u"]["probabilities"], json!({"0": 0.0, "1": 1.0, "2": 0.0}));
        assert_eq!(json["answers"]["u"]["score"], json!(1.0));
    }

    #[test]
    fn cassette_round_trip() {
        let http = HttpResponse { status: StatusCode::OK, headers: HeaderMap::new(), body: br#"{"a":1}"#.to_vec() };
        let recorded = CassetteResponse::from_http(&http);
        assert_eq!(recorded.json, Some(json!({"a": 1})));
        let text = serde_json::to_string(&Cassette {
            exchanges: vec![Exchange {
                request: CassetteRequest { method: "POST".into(), url: "http://x/v1/systemone".into(), body: None },
                response: recorded,
            }],
        })
        .unwrap();
        let cassette: Cassette = serde_json::from_str(&text).unwrap();
        let mock = MockTransport::replay(cassette);
        assert_eq!(mock.pending(), 1);
    }

    #[test]
    fn recorded_request_helpers() {
        let request = RecordedRequest {
            method: "POST".into(),
            url: "http://mock.typesafe.invalid/v1/systemone".into(),
            headers: HeaderMap::new(),
            body: Some(json!({"state": "s", "model": "m", "questions": {"q": {"type": "noul"}}})),
            attempt: 0,
        };
        assert_eq!(request.path(), "/v1/systemone");
        assert_eq!(request.state(), Some(&json!("s")));
        assert_eq!(request.model(), Some("m"));
        assert_eq!(request.question("q").unwrap()["type"], json!("noul"));
        assert!(request.question("zzz").is_none());
    }
}
