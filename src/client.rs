//! The asynchronous client and its request builders.

use std::collections::BTreeMap;
use std::fmt;
use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Method;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::config::Config;
use crate::constants::{MODELS_PATH, SYSTEM_ONE_PATH};
use crate::error::{Error, Result};
use crate::question::{Question, validate_questions};
use crate::response::{ListModelsResponse, RawResponse, SystemOneResponse};
use crate::retry::RetryPolicy;
use crate::transport::{Custom, PreparedRequest, prepare, send};

// --- Shared option handling -------------------------------------------------------------------

/// Options common to the async and blocking client builders.
#[derive(Default)]
pub(crate) struct ClientOptions {
    pub(crate) api_key: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) retry: Option<RetryPolicy>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) headers: HeaderMap,
    pub(crate) base_url: Option<String>,
    pub(crate) error: Option<Error>,
}

impl ClientOptions {
    pub(crate) fn resolve(self) -> Result<(Config, RetryPolicy)> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let retry = self.retry.unwrap_or_default();
        retry.validate()?;
        let config = Config::resolve(self.api_key, self.base_url, self.model, self.timeout, self.headers)?;
        Ok((config, retry))
    }
}

/// Insert a header, recording the first invalid name or value as a deferred error.
pub(crate) fn insert_header<N, V>(headers: &mut HeaderMap, error: &mut Option<Error>, name: N, value: V)
where
    N: TryInto<HeaderName>,
    N::Error: fmt::Display,
    V: TryInto<HeaderValue>,
    V::Error: fmt::Display,
{
    match (name.try_into(), value.try_into()) {
        (Ok(name), Ok(value)) => {
            headers.insert(name, value);
        }
        (Err(problem), _) => {
            error.get_or_insert_with(|| Error::Config(format!("Invalid header name: {problem}")));
        }
        (_, Err(problem)) => {
            error.get_or_insert_with(|| Error::Config(format!("Invalid header value: {problem}")));
        }
    }
}

/// Generates the option setters shared by the async and blocking client builders.
macro_rules! client_builder_options {
    () => {
        /// Required API key; may instead be set via the `TYPESAFE_API_KEY` environment variable.
        pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
            self.options.api_key = Some(api_key.into());
            self
        }

        /// Default model name; may instead be set via the `TYPESAFE_DEFAULT_MODEL` environment
        /// variable. Defaults to `jev-latest`.
        pub fn model(mut self, model: impl Into<String>) -> Self {
            self.options.model = Some(model.into());
            self
        }

        /// A [`RetryPolicy`] controlling retry behavior. Pass [`RetryPolicy::none`] to disable
        /// retries.
        pub fn retry(mut self, retry: RetryPolicy) -> Self {
            self.options.retry = Some(retry);
            self
        }

        /// Timeout for each HTTP operation. Defaults to 10 seconds.
        pub fn timeout(mut self, timeout: Duration) -> Self {
            self.options.timeout = Some(timeout);
            self
        }

        /// An additional header sent with every request. Authentication, SDK identification,
        /// and `Accept` headers are protected and cannot be overridden.
        pub fn header<N, V>(mut self, name: N, value: V) -> Self
        where
            N: TryInto<HeaderName>,
            N::Error: fmt::Display,
            V: TryInto<HeaderValue>,
            V::Error: fmt::Display,
        {
            insert_header(&mut self.options.headers, &mut self.options.error, name, value);
            self
        }

        /// Additional headers sent with every request; see [`header`](Self::header).
        pub fn headers(mut self, headers: HeaderMap) -> Self {
            self.options.headers.extend(headers);
            self
        }

        /// API root; may instead be set via the `TYPESAFE_BASE_URL` environment variable.
        /// Defaults to `https://api.typesafe.ai`.
        pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
            self.options.base_url = Some(base_url.into());
            self
        }
    };
}
#[cfg(feature = "blocking")]
pub(crate) use client_builder_options;

// --- Async client -----------------------------------------------------------------------------

struct Inner {
    config: Config,
    retry: RetryPolicy,
    http: reqwest::Client,
}

/// An asynchronous HTTP client for the [TypeSafe AI API](https://typesafe.ai).
///
/// The client is cheap to clone and shares its connection pool between clones.
///
/// ```no_run
/// use typesafeai_sdk_community::{Choice, Noul, TypeSafeClient};
///
/// # async fn run() -> typesafeai_sdk_community::Result<()> {
/// let client = TypeSafeClient::new()?; // reads TYPESAFE_API_KEY
/// let result = client
///     .system_one("I was charged twice. Please help.")
///     .question("billing", Noul::new("Is this about billing?"))
///     .question("tone", Choice::new("What is the tone?").labels(["calm", "angry"]))
///     .send()
///     .await?;
/// assert!((0.0..=1.0).contains(&result.noul("billing").unwrap().noul));
/// assert!(["calm", "angry"].contains(&result.choice("tone").unwrap().choice.as_str()));
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct TypeSafeClient {
    inner: Arc<Inner>,
}

impl fmt::Debug for TypeSafeClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypeSafeClient").field("config", &self.inner.config).field("retry", &self.inner.retry).finish()
    }
}

impl TypeSafeClient {
    /// Create a client configured entirely from the environment.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] if `TYPESAFE_API_KEY` is unset.
    pub fn new() -> Result<Self> {
        Self::builder().build()
    }

    /// Start configuring a client.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// Answer named questions about text or structured state.
    ///
    /// Add questions with [`question`](SystemOneRequest::question), adjust per-call options,
    /// then [`send`](SystemOneRequest::send) (or `.await` the builder directly).
    ///
    /// See [System One](https://docs.typesafe.ai/concepts/system-one) and
    /// [state](https://docs.typesafe.ai/concepts/state) for details.
    pub fn system_one(&self, state: impl Into<Value>) -> SystemOneRequest<'_, Self> {
        SystemOneRequest { client: self, params: SystemOneParams::new(state.into()) }
    }

    /// Access the Models API resource.
    pub fn models(&self) -> Models<'_, Self> {
        Models { client: self }
    }

    /// The model used when a request does not name one.
    pub fn default_model(&self) -> &str {
        &self.inner.config.default_model
    }

    /// The API root this client sends to.
    pub fn base_url(&self) -> &str {
        &self.inner.config.base_url
    }

    /// The underlying `reqwest` client.
    pub fn http_client(&self) -> &reqwest::Client {
        &self.inner.http
    }

    async fn dispatch<T: crate::transport::Decode>(
        &self,
        request: PreparedRequest,
        retry: Option<&RetryPolicy>,
    ) -> Result<T> {
        let policy = match retry {
            Some(policy) => {
                policy.validate()?;
                policy
            }
            None => &self.inner.retry,
        };
        send(&self.inner.http, request, policy).await
    }
}

/// Configures a [`TypeSafeClient`].
///
/// Explicit options take precedence over environment variables; empty or whitespace-only
/// environment values are ignored.
#[derive(Default)]
pub struct ClientBuilder {
    options: ClientOptions,
    http_client: Option<reqwest::Client>,
}

impl ClientBuilder {
    client_builder_options!();

    /// A preconfigured `reqwest::Client` to send with, for custom TLS, proxies, or connection
    /// pooling. The SDK still applies its per-request timeout.
    pub fn http_client(mut self, http_client: reqwest::Client) -> Self {
        self.http_client = Some(http_client);
        self
    }

    /// Build the client.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] if the API key is missing, the timeout or retry policy is invalid, a
    /// header is invalid, or the HTTP client cannot be initialized.
    pub fn build(self) -> Result<TypeSafeClient> {
        let (config, retry) = self.options.resolve()?;
        let http = match self.http_client {
            Some(http) => http,
            None => reqwest::Client::builder()
                .timeout(config.timeout)
                .build()
                .map_err(|error| Error::Config(format!("Could not initialize the HTTP client: {error}")))?,
        };
        Ok(TypeSafeClient { inner: Arc::new(Inner { config, retry, http }) })
    }
}

// --- System One request builder ---------------------------------------------------------------

pub(crate) struct SystemOneParams {
    state: Value,
    questions: BTreeMap<String, Question>,
    model: Option<String>,
    pub(crate) retry: Option<RetryPolicy>,
    timeout: Option<Duration>,
    headers: HeaderMap,
    extra_body: Map<String, Value>,
    error: Option<Error>,
}

impl SystemOneParams {
    pub(crate) fn new(state: Value) -> Self {
        SystemOneParams {
            state,
            questions: BTreeMap::new(),
            model: None,
            retry: None,
            timeout: None,
            headers: HeaderMap::new(),
            extra_body: Map::new(),
            error: None,
        }
    }

    pub(crate) fn prepare(&mut self, config: &Config) -> Result<PreparedRequest> {
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        validate_questions(&self.questions)?;
        let mut body = Map::new();
        body.insert("state".into(), self.state.clone());
        body.insert("model".into(), Value::String(self.model.clone().unwrap_or_else(|| config.default_model.clone())));
        body.insert(
            "questions".into(),
            serde_json::to_value(&self.questions).map_err(|error| {
                Error::InvalidRequest(format!("The request body could not be encoded as JSON: {error}"))
            })?,
        );
        for (key, value) in &self.extra_body {
            body.insert(key.clone(), value.clone());
        }
        prepare(config, Method::POST, SYSTEM_ONE_PATH, Some(&body), self.timeout, &self.headers)
    }
}

/// A System One request under construction; see [`TypeSafeClient::system_one`].
#[must_use = "a request does nothing until it is sent"]
pub struct SystemOneRequest<'a, C> {
    pub(crate) client: &'a C,
    pub(crate) params: SystemOneParams,
}

impl<C> SystemOneRequest<'_, C> {
    /// Add a question under the name its answer will be keyed by.
    pub fn question(mut self, name: impl Into<String>, question: impl Into<Question>) -> Self {
        self.params.questions.insert(name.into(), question.into());
        self
    }

    /// Add several named questions.
    pub fn questions(mut self, questions: impl IntoIterator<Item = (impl Into<String>, impl Into<Question>)>) -> Self {
        for (name, question) in questions {
            self.params.questions.insert(name.into(), question.into());
        }
        self
    }

    /// Model override for this call; otherwise the client default is used.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.params.model = Some(model.into());
        self
    }

    /// A retry policy overriding the client-level value for this call only.
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.params.retry = Some(retry);
        self
    }

    /// An HTTP timeout overriding the client-level value for this call only.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.params.timeout = Some(timeout);
        self
    }

    /// An additional request header for this call. Authentication, SDK identification, and
    /// `Accept` headers are protected and cannot be overridden.
    pub fn header<N, V>(mut self, name: N, value: V) -> Self
    where
        N: TryInto<HeaderName>,
        N::Error: fmt::Display,
        V: TryInto<HeaderValue>,
        V::Error: fmt::Display,
    {
        insert_header(&mut self.params.headers, &mut self.params.error, name, value);
        self
    }

    /// An additional top-level request-body field, shallow-merged over the body after `state`,
    /// `model`, and `questions` are set. Merging is last-write-wins: a key that collides with
    /// those fields overrides it, and object values are replaced rather than deep-merged.
    pub fn extra_body(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.params.extra_body.insert(key.into(), value.into());
        self
    }
}

impl<'a> SystemOneRequest<'a, TypeSafeClient> {
    /// Send the request and decode the answers.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidRequest`]: no questions, or a score question with no criteria.
    /// - [`Error::Api`]: the server returned an unsuccessful response after any retries.
    /// - [`Error::Connection`] / [`Error::Timeout`]: the request could not complete after any
    ///   retries.
    /// - [`Error::ResponseValidation`]: the response body did not match the expected schema.
    pub async fn send(mut self) -> Result<SystemOneResponse> {
        let request = self.params.prepare(&self.client.inner.config)?;
        self.client.dispatch(request, self.params.retry.as_ref()).await
    }

    /// Send the request and decode the JSON body into any `serde` type describing the response,
    /// including any nested answer models.
    pub async fn send_as<T: DeserializeOwned>(mut self) -> Result<T> {
        let request = self.params.prepare(&self.client.inner.config)?;
        self.client.dispatch::<Custom<T>>(request, self.params.retry.as_ref()).await.map(|custom| custom.0)
    }

    /// Send the request and return the successful response undecoded.
    pub async fn send_raw(mut self) -> Result<RawResponse> {
        let request = self.params.prepare(&self.client.inner.config)?;
        self.client.dispatch(request, self.params.retry.as_ref()).await
    }
}

impl<'a> IntoFuture for SystemOneRequest<'a, TypeSafeClient> {
    type Output = Result<SystemOneResponse>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.send())
    }
}

// --- Models resource --------------------------------------------------------------------------

/// Access to the models available to the account; see [`TypeSafeClient::models`].
pub struct Models<'a, C> {
    pub(crate) client: &'a C,
}

impl<'a, C> Models<'a, C> {
    /// List the models available to the account.
    pub fn list(&self) -> ListModelsRequest<'a, C> {
        ListModelsRequest { client: self.client, params: ListModelsParams::default() }
    }
}

#[derive(Default)]
pub(crate) struct ListModelsParams {
    pub(crate) retry: Option<RetryPolicy>,
    timeout: Option<Duration>,
    headers: HeaderMap,
    error: Option<Error>,
}

impl ListModelsParams {
    pub(crate) fn prepare(&mut self, config: &Config) -> Result<PreparedRequest> {
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        prepare::<()>(config, Method::GET, MODELS_PATH, None, self.timeout, &self.headers)
    }
}

/// A models listing request under construction; see [`Models::list`].
#[must_use = "a request does nothing until it is sent"]
pub struct ListModelsRequest<'a, C> {
    pub(crate) client: &'a C,
    pub(crate) params: ListModelsParams,
}

impl<C> ListModelsRequest<'_, C> {
    /// A retry policy overriding the client-level value for this call only.
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.params.retry = Some(retry);
        self
    }

    /// An HTTP timeout overriding the client-level value for this call only.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.params.timeout = Some(timeout);
        self
    }

    /// An additional request header for this call.
    pub fn header<N, V>(mut self, name: N, value: V) -> Self
    where
        N: TryInto<HeaderName>,
        N::Error: fmt::Display,
        V: TryInto<HeaderValue>,
        V::Error: fmt::Display,
    {
        insert_header(&mut self.params.headers, &mut self.params.error, name, value);
        self
    }
}

impl<'a> ListModelsRequest<'a, TypeSafeClient> {
    /// Send the request.
    ///
    /// # Errors
    ///
    /// [`Error::Api`], [`Error::Connection`], [`Error::Timeout`], or
    /// [`Error::ResponseValidation`], as for [`SystemOneRequest::send`].
    pub async fn send(mut self) -> Result<ListModelsResponse> {
        let request = self.params.prepare(&self.client.inner.config)?;
        self.client.dispatch(request, self.params.retry.as_ref()).await
    }
}

impl<'a> IntoFuture for ListModelsRequest<'a, TypeSafeClient> {
    type Output = Result<ListModelsResponse>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.send())
    }
}
