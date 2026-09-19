//! A synchronous client for non-async code. Enable the `blocking` Cargo feature to use it.
//!
//! The blocking client must not be used from within an async runtime such as `tokio`; it
//! panics there, exactly as `reqwest::blocking` does. Use [`crate::TypeSafeClient`] instead.
//!
//! ```no_run
//! use typesafeai_sdk_community::blocking::TypeSafeClient;
//! use typesafeai_sdk_community::{Choice, Noul};
//!
//! # fn run() -> typesafeai_sdk_community::Result<()> {
//! let client = TypeSafeClient::new()?; // reads TYPESAFE_API_KEY
//! let result = client
//!     .system_one("I was charged twice. Please help.")
//!     .question("billing", Noul::new("Is this about billing?"))
//!     .question("tone", Choice::new("What is the tone?").labels(["calm", "angry"]))
//!     .send()?;
//! println!("billing: {:.2}", result.noul("billing").unwrap().noul);
//! # Ok(())
//! # }
//! ```

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::client::{
    AskRequest, ClientOptions, ListModelsRequest, Models, RouteRequest, SystemOneParams, SystemOneRequest,
    client_builder_options, insert_header,
};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::response::{ListModelsResponse, RawResponse, SystemOneResponse};
use crate::retry::RetryPolicy;
use crate::transport::{BlockingTransport, Custom, Decode, PreparedRequest, ReqwestBlockingTransport, send_blocking};
use crate::typed::{Answered, Questions, Route, Routed};

struct Inner {
    config: Config,
    retry: RetryPolicy,
    transport: Arc<dyn BlockingTransport>,
}

/// A synchronous HTTP client for the [TypeSafe AI API](https://typesafe.ai).
///
/// The client is cheap to clone and shares its connection pool between clones.
#[derive(Clone)]
pub struct TypeSafeClient {
    inner: Arc<Inner>,
}

impl fmt::Debug for TypeSafeClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("blocking::TypeSafeClient")
            .field("config", &self.inner.config)
            .field("retry", &self.inner.retry)
            .finish()
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

    /// Answer named questions about text or structured state; see
    /// [`TypeSafeClient::system_one`](crate::TypeSafeClient::system_one).
    pub fn system_one(&self, state: impl Into<Value>) -> SystemOneRequest<'_, Self> {
        SystemOneRequest { client: self, params: SystemOneParams::new(state.into()) }
    }

    /// Ask a typed question set; see [`TypeSafeClient::ask`](crate::TypeSafeClient::ask).
    pub fn ask<T: Questions>(&self, state: impl Into<Value>) -> AskRequest<'_, Self, T> {
        AskRequest { inner: self.system_one(state).questions(T::questions()), _answers: std::marker::PhantomData }
    }

    /// Route to one variant of a [`Route`] enum; see
    /// [`TypeSafeClient::route`](crate::TypeSafeClient::route).
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
    pub fn http_client(&self) -> Option<&reqwest::blocking::Client> {
        self.inner.transport.reqwest_client()
    }

    /// The transport requests are sent through.
    pub fn transport(&self) -> &dyn BlockingTransport {
        self.inner.transport.as_ref()
    }

    fn dispatch<T: Decode>(&self, request: PreparedRequest, retry: Option<&RetryPolicy>) -> Result<T> {
        let policy = match retry {
            Some(policy) => {
                policy.validate()?;
                policy
            }
            None => &self.inner.retry,
        };
        send_blocking(self.inner.transport.as_ref(), request, policy)
    }
}

/// Configures a blocking [`TypeSafeClient`].
///
/// Explicit options take precedence over environment variables; empty or whitespace-only
/// environment values are ignored.
#[derive(Default)]
pub struct ClientBuilder {
    options: ClientOptions,
    http_client: Option<reqwest::blocking::Client>,
    transport: Option<Arc<dyn BlockingTransport>>,
}

impl ClientBuilder {
    client_builder_options!();

    /// A preconfigured `reqwest::blocking::Client` to send with. The SDK still applies its
    /// per-request timeout.
    pub fn http_client(mut self, http_client: reqwest::blocking::Client) -> Self {
        self.http_client = Some(http_client);
        self
    }

    /// A custom [`BlockingTransport`] to send through instead of `reqwest`. Takes precedence
    /// over `http_client`.
    pub fn transport(mut self, transport: impl BlockingTransport) -> Self {
        self.transport = Some(Arc::new(transport));
        self
    }

    /// A shared custom [`BlockingTransport`]; see [`transport`](Self::transport).
    pub fn transport_arc(mut self, transport: Arc<dyn BlockingTransport>) -> Self {
        self.transport = Some(transport);
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
        let transport: Arc<dyn BlockingTransport> = match (self.transport, self.http_client) {
            (Some(transport), _) => transport,
            (None, Some(http)) => Arc::new(ReqwestBlockingTransport::new(http)),
            (None, None) => Arc::new(ReqwestBlockingTransport::new(
                reqwest::blocking::Client::builder()
                    .timeout(config.timeout)
                    .build()
                    .map_err(|error| Error::Config(format!("Could not initialize the HTTP client: {error}")))?,
            )),
        };
        Ok(TypeSafeClient { inner: Arc::new(Inner { config, retry, transport }) })
    }
}

impl SystemOneRequest<'_, TypeSafeClient> {
    /// Send the request and decode the answers; see
    /// [`SystemOneRequest::send`](crate::SystemOneRequest::send) for the errors.
    pub fn send(mut self) -> Result<SystemOneResponse> {
        let request = self.params.prepare(&self.client.inner.config)?;
        self.client.dispatch(request, self.params.retry.as_ref())
    }

    /// Send the request and decode the JSON body into any `serde` type describing the response.
    pub fn send_as<T: DeserializeOwned>(mut self) -> Result<T> {
        let request = self.params.prepare(&self.client.inner.config)?;
        self.client.dispatch::<Custom<T>>(request, self.params.retry.as_ref()).map(|custom| custom.0)
    }

    /// Send the request and return the successful response undecoded.
    pub fn send_raw(mut self) -> Result<RawResponse> {
        let request = self.params.prepare(&self.client.inner.config)?;
        self.client.dispatch(request, self.params.retry.as_ref())
    }
}

impl<T: Questions> AskRequest<'_, TypeSafeClient, T> {
    /// Send the request and parse the answers into `T`.
    pub fn send(self) -> Result<T> {
        Ok(self.send_full()?.answers)
    }

    /// Send the request and return the parsed `T` together with the full response.
    pub fn send_full(self) -> Result<Answered<T>> {
        let response = self.inner.send()?;
        let answers = T::from_response(&response)?;
        Ok(Answered { answers, response })
    }
}

impl<T: Route> RouteRequest<'_, TypeSafeClient, T> {
    /// Send the request and return the selected variant.
    pub fn send(self) -> Result<T> {
        Ok(self.send_full()?.route)
    }

    /// Send the request and return the variant with the routing choice and full response.
    pub fn send_full(self) -> Result<Routed<T>> {
        let response = self.inner.inner.send()?;
        Ok(Routed::from_response(response)?)
    }
}

impl ListModelsRequest<'_, TypeSafeClient> {
    /// Send the request.
    pub fn send(mut self) -> Result<ListModelsResponse> {
        let request = self.params.prepare(&self.client.inner.config)?;
        self.client.dispatch(request, self.params.retry.as_ref())
    }
}
