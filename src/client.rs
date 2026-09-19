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
use crate::transport::{Custom, PreparedRequest, ReqwestTransport, Transport, prepare, send};
use crate::typed::{Answered, Questions, Route, Routed};

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
    pub(crate) check_paths: bool,
    pub(crate) error: Option<Error>,
}

impl ClientOptions {
    pub(crate) fn resolve(self) -> Result<(Config, RetryPolicy)> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let retry = self.retry.unwrap_or_default();
        retry.validate()?;
        let mut config = Config::resolve(self.api_key, self.base_url, self.model, self.timeout, self.headers)?;
        config.check_paths = self.check_paths;
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

        /// Check every request's questions for backticked state paths that the state does not
        /// contain, and fail with [`Error::StatePath`](crate::Error::StatePath) instead of
        /// sending; see the [`state`](crate::state) module. Off by default; a request can
        /// override it with `.check_paths()` / `.skip_path_check()`.
        pub fn check_paths(mut self, enabled: bool) -> Self {
            self.options.check_paths = enabled;
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
    transport: Arc<dyn Transport>,
    cache: Option<crate::cache::Cache>,
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

    /// Ask a typed question set: every field of `T` is sent as one request and the answers come
    /// back as a `T`. See the [`typed`](crate::typed) module.
    ///
    /// ```no_run
    /// # use typesafeai_sdk_community::{NoulAnswer, Questions, TypeSafeClient};
    /// #[derive(Questions)]
    /// struct Check {
    ///     #[noul("Is this message spam?")]
    ///     spam: NoulAnswer,
    /// }
    /// # async fn run(client: TypeSafeClient) -> typesafeai_sdk_community::Result<()> {
    /// let check = client.ask::<Check>("Buy now!!!").send().await?;
    /// println!("{:.2}", check.spam.noul);
    /// # Ok(())
    /// # }
    /// ```
    pub fn ask<T: Questions>(&self, state: impl Into<Value>) -> AskRequest<'_, Self, T> {
        AskRequest { inner: self.system_one(state).questions(T::questions()), _answers: std::marker::PhantomData }
    }

    /// Route to one variant of a [`Route`] enum and fill its fields, in one request. See the
    /// [`typed`](crate::typed) module.
    ///
    /// ```no_run
    /// # use typesafeai_sdk_community::{Route, TypeSafeClient};
    /// #[derive(Debug, Route)]
    /// #[route("What does the customer want?")]
    /// enum Intent {
    ///     #[route(describe = "Wants money back")]
    ///     Refund {
    ///         #[noul("Is the full amount requested?")]
    ///         full_amount: bool,
    ///     },
    ///     #[route(describe = "Nothing above applies")]
    ///     Other,
    /// }
    /// # async fn run(client: TypeSafeClient) -> typesafeai_sdk_community::Result<()> {
    /// match client.route::<Intent>("I want all my money back").send().await? {
    ///     Intent::Refund { full_amount } => println!("refund, full={full_amount}"),
    ///     Intent::Other => println!("something else"),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn route<T: Route>(&self, state: impl Into<Value>) -> RouteRequest<'_, Self, T> {
        RouteRequest { inner: self.ask::<T>(state) }
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

    /// The underlying `reqwest` client, unless a custom transport was supplied.
    pub fn http_client(&self) -> Option<&reqwest::Client> {
        self.inner.transport.reqwest_client()
    }

    /// The transport requests are sent through.
    pub fn transport(&self) -> &dyn Transport {
        self.inner.transport.as_ref()
    }

    /// A client sharing this one's configuration whose transport waits on `pacer`.
    pub(crate) fn paced(&self, pacer: crate::batch::Pacer) -> TypeSafeClient {
        let transport = crate::batch::PacedTransport::new(Arc::clone(&self.inner.transport), pacer);
        TypeSafeClient {
            inner: Arc::new(Inner {
                config: self.inner.config.clone(),
                retry: self.inner.retry.clone(),
                transport: Arc::new(transport),
                cache: self.inner.cache.clone(),
            }),
        }
    }

    /// The response cache, if one was configured; see the [`cache`](crate::cache) module.
    pub fn cache(&self) -> Option<&crate::cache::Cache> {
        self.inner.cache.as_ref()
    }

    async fn dispatch<T: crate::transport::Decode>(
        &self,
        request: PreparedRequest,
        retry: Option<&RetryPolicy>,
        cache: &crate::transport::CacheUse,
    ) -> Result<T> {
        let policy = match retry {
            Some(policy) => {
                policy.validate()?;
                policy
            }
            None => &self.inner.retry,
        };
        let key = match crate::transport::cache_lookup::<T>(self.inner.cache.as_ref(), &request, cache)? {
            Ok(hit) => return Ok(hit),
            Err(key) => key,
        };
        let raw = send(self.inner.transport.as_ref(), &request, policy).await?;
        if let (Some(cache), Some(key)) = (&self.inner.cache, key) {
            cache.store_response(key, &raw);
        }
        T::decode(&request, raw)
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
    transport: Option<Arc<dyn Transport>>,
    cache: Option<crate::cache::Cache>,
}

impl ClientBuilder {
    client_builder_options!();

    /// A preconfigured `reqwest::Client` to send with, for custom TLS, proxies, or connection
    /// pooling. The SDK still applies its per-request timeout.
    pub fn http_client(mut self, http_client: reqwest::Client) -> Self {
        self.http_client = Some(http_client);
        self
    }

    /// A custom [`Transport`] to send through instead of `reqwest`: your own HTTP stack, a
    /// recorder, or a mock (see the `testing` module). Takes precedence over `http_client`.
    pub fn transport(mut self, transport: impl Transport) -> Self {
        self.transport = Some(Arc::new(transport));
        self
    }

    /// A shared custom [`Transport`]; see [`transport`](Self::transport).
    pub fn transport_arc(mut self, transport: Arc<dyn Transport>) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Serve repeated System One requests from a [`Cache`](crate::cache::Cache) instead of
    /// the API; see the [`cache`](crate::cache) module.
    pub fn cache(mut self, cache: crate::cache::Cache) -> Self {
        self.cache = Some(cache);
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
        let transport: Arc<dyn Transport> = match (self.transport, self.http_client) {
            (Some(transport), _) => transport,
            (None, Some(http)) => Arc::new(ReqwestTransport::new(http)),
            (None, None) => Arc::new(ReqwestTransport::new(
                reqwest::Client::builder()
                    .timeout(config.timeout)
                    .build()
                    .map_err(|error| Error::Config(format!("Could not initialize the HTTP client: {error}")))?,
            )),
        };
        Ok(TypeSafeClient { inner: Arc::new(Inner { config, retry, transport, cache: self.cache }) })
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
    check_paths: Option<bool>,
    pub(crate) cache: crate::transport::CacheUse,
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
            check_paths: None,
            cache: crate::transport::CacheUse::default(),
            error: None,
        }
    }

    pub(crate) fn prepare(&mut self, config: &Config) -> Result<PreparedRequest> {
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        validate_questions(&self.questions)?;
        if self.check_paths.unwrap_or(config.check_paths) {
            let issues = crate::state::check(&self.state, &self.questions);
            if !issues.is_empty() {
                return Err(crate::state::StatePathError { issues }.into());
            }
        }
        // `prepare` is the last use of the state and extra fields, so move them instead of
        // cloning the (possibly large) document on every request.
        let mut body = Map::new();
        body.insert("state".into(), std::mem::take(&mut self.state));
        body.insert("model".into(), Value::String(self.model.take().unwrap_or_else(|| config.default_model.clone())));
        body.insert(
            "questions".into(),
            serde_json::to_value(&self.questions).map_err(|error| {
                Error::InvalidRequest(format!("The request body could not be encoded as JSON: {error}"))
            })?,
        );
        for (key, value) in std::mem::take(&mut self.extra_body) {
            body.insert(key, value);
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

    /// Fail before sending if a question references a backticked state path the state does
    /// not contain; see the [`state`](crate::state) module.
    pub fn check_paths(mut self) -> Self {
        self.params.check_paths = Some(true);
        self
    }

    /// Skip the state path check for this call even if the client enables it.
    pub fn skip_path_check(mut self) -> Self {
        self.params.check_paths = Some(false);
        self
    }

    /// Add a dimension to the cache key for this call, such as a tenant or user, without
    /// changing the request. Only meaningful when the client has a cache.
    pub fn cache_scope(mut self, scope: impl Into<String>) -> Self {
        self.params.cache.scope = Some(scope.into());
        self
    }

    /// Skip the cache read for this call but store the fresh response.
    pub fn refresh(mut self) -> Self {
        self.params.cache.mode = crate::cache::CacheMode::Refresh;
        self
    }

    /// Neither read from nor write to the cache for this call.
    pub fn no_cache(mut self) -> Self {
        self.params.cache.mode = crate::cache::CacheMode::Bypass;
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
        self.client.dispatch(request, self.params.retry.as_ref(), &self.params.cache).await
    }

    /// Send the request and decode the JSON body into any `serde` type describing the response,
    /// including any nested answer models.
    pub async fn send_as<T: DeserializeOwned>(mut self) -> Result<T> {
        let request = self.params.prepare(&self.client.inner.config)?;
        self.client
            .dispatch::<Custom<T>>(request, self.params.retry.as_ref(), &self.params.cache)
            .await
            .map(|custom| custom.0)
    }

    /// Send the request and return the successful response undecoded.
    pub async fn send_raw(mut self) -> Result<RawResponse> {
        let request = self.params.prepare(&self.client.inner.config)?;
        self.client.dispatch(request, self.params.retry.as_ref(), &self.params.cache).await
    }
}

impl<'a> IntoFuture for SystemOneRequest<'a, TypeSafeClient> {
    type Output = Result<SystemOneResponse>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.send())
    }
}

// --- Typed question-set request -----------------------------------------------------------------

/// A [`Questions`] request under construction; see [`TypeSafeClient::ask`].
///
/// Accepts the same per-call options as [`SystemOneRequest`], plus extra ad-hoc questions with
/// [`question`](Self::question) that end up in the full response but not in `T`.
#[must_use = "a request does nothing until it is sent"]
pub struct AskRequest<'a, C, T> {
    pub(crate) inner: SystemOneRequest<'a, C>,
    pub(crate) _answers: std::marker::PhantomData<fn() -> T>,
}

/// Per-call options forwarded to an inner request builder, so every typed builder offers the
/// same options as [`SystemOneRequest`] without hand-copying them.
macro_rules! delegate_request_options {
    () => {
        /// Add an ad-hoc question alongside the typed set. It is available through the full
        /// response but not as a typed field.
        pub fn question(mut self, name: impl Into<String>, question: impl Into<Question>) -> Self {
            self.inner = self.inner.question(name, question);
            self
        }

        /// Model override for this call; otherwise the client default is used.
        pub fn model(mut self, model: impl Into<String>) -> Self {
            self.inner = self.inner.model(model);
            self
        }

        /// A retry policy overriding the client-level value for this call only.
        pub fn retry(mut self, retry: RetryPolicy) -> Self {
            self.inner = self.inner.retry(retry);
            self
        }

        /// An HTTP timeout overriding the client-level value for this call only.
        pub fn timeout(mut self, timeout: Duration) -> Self {
            self.inner = self.inner.timeout(timeout);
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
            self.inner = self.inner.header(name, value);
            self
        }

        /// An additional top-level request-body field; see [`SystemOneRequest::extra_body`].
        pub fn extra_body(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
            self.inner = self.inner.extra_body(key, value);
            self
        }

        /// Fail before sending if a question references a state path the state does not
        /// contain; see the [`state`](crate::state) module.
        pub fn check_paths(mut self) -> Self {
            self.inner = self.inner.check_paths();
            self
        }

        /// Skip the state path check for this call even if the client enables it.
        pub fn skip_path_check(mut self) -> Self {
            self.inner = self.inner.skip_path_check();
            self
        }

        /// Add a dimension to the cache key for this call; see [`SystemOneRequest::cache_scope`].
        pub fn cache_scope(mut self, scope: impl Into<String>) -> Self {
            self.inner = self.inner.cache_scope(scope);
            self
        }

        /// Skip the cache read for this call but store the fresh response.
        pub fn refresh(mut self) -> Self {
            self.inner = self.inner.refresh();
            self
        }

        /// Neither read from nor write to the cache for this call.
        pub fn no_cache(mut self) -> Self {
            self.inner = self.inner.no_cache();
            self
        }
    };
}

impl<'a, C, T: Questions> AskRequest<'a, C, T> {
    delegate_request_options!();

    /// The underlying untyped request, for anything not exposed here.
    pub fn into_inner(self) -> SystemOneRequest<'a, C> {
        self.inner
    }
}

impl<'a, T: Questions> AskRequest<'a, TypeSafeClient, T> {
    /// Send the request and parse the answers into `T`.
    ///
    /// # Errors
    ///
    /// Everything [`SystemOneRequest::send`] can return, plus [`Error::Answer`] when the
    /// response cannot be converted into `T`.
    pub async fn send(self) -> Result<T> {
        Ok(self.send_full().await?.answers)
    }

    /// Send the request and return the parsed `T` together with the full response.
    pub async fn send_full(self) -> Result<Answered<T>> {
        let response = self.inner.send().await?;
        let answers = T::from_response(&response)?;
        Ok(Answered { answers, response })
    }
}

impl<'a, T: Questions + 'a> IntoFuture for AskRequest<'a, TypeSafeClient, T> {
    type Output = Result<T>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.send())
    }
}

// --- Typed routing request ----------------------------------------------------------------------

/// A [`Route`] request under construction; see [`TypeSafeClient::route`]. Accepts the same
/// per-call options as [`AskRequest`].
#[must_use = "a request does nothing until it is sent"]
pub struct RouteRequest<'a, C, T> {
    pub(crate) inner: AskRequest<'a, C, T>,
}

impl<'a, C, T: Route> RouteRequest<'a, C, T> {
    delegate_request_options!();

    /// The underlying typed request.
    pub fn into_inner(self) -> AskRequest<'a, C, T> {
        self.inner
    }
}

impl<'a, T: Route> RouteRequest<'a, TypeSafeClient, T> {
    /// Send the request and return the selected variant.
    pub async fn send(self) -> Result<T> {
        Ok(self.send_full().await?.route)
    }

    /// Send the request and return the variant with the routing choice and full response.
    pub async fn send_full(self) -> Result<Routed<T>> {
        let response = self.inner.inner.send().await?;
        Ok(Routed::from_response(response)?)
    }
}

impl<'a, T: Route + 'a> IntoFuture for RouteRequest<'a, TypeSafeClient, T> {
    type Output = Result<T>;
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
        self.client.dispatch(request, self.params.retry.as_ref(), &crate::transport::CacheUse::default()).await
    }
}

impl<'a> IntoFuture for ListModelsRequest<'a, TypeSafeClient> {
    type Output = Result<ListModelsResponse>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.send())
    }
}
