//! Community-built Rust SDK for the [TypeSafe AI](https://typesafe.ai) API.
//!
//! TypeSafe's **System One** models (Jev is the flagship) return fast, typed judgments
//! (probabilities, selections, and scores) that code can consume directly. This crate is a port
//! of the official [Python SDK](https://github.com/typesafe-ai/typesafe-sdk-python): same
//! primitives, same request/response shapes, same retry and error semantics, in idiomatic Rust.
//!
//! # Quickstart
//!
//! Set `TYPESAFE_API_KEY` in your environment, then:
//!
//! ```no_run
//! use typesafe_sdk::{Choice, Noul, Score, TypeSafeClient, json};
//!
//! #[tokio::main]
//! async fn main() -> typesafe_sdk::Result<()> {
//!     let client = TypeSafeClient::new()?;
//!
//!     let response = client
//!         .system_one(json!({"document": "I was charged twice. Please help."}))
//!         .question("billing", Noul::new("Is this message about billing?"))
//!         .question(
//!             "tone",
//!             Choice::new("What is the tone of this message?")
//!                 .option("angry", "An upset or hostile message")
//!                 .option("calm", "A neutral or polite message"),
//!         )
//!         .question(
//!             "urgency",
//!             Score::new("How urgent is this?", ["Can wait", "This week", "Today"]),
//!         )
//!         .send()
//!         .await?;
//!
//!     let billing = response.noul("billing").unwrap();
//!     let tone = response.choice("tone").unwrap();
//!     let urgency = response.score("urgency").unwrap();
//!     println!("billing={:.2} tone={} ({:.2}) urgency={:.1}", billing.noul, tone.choice, tone.confidence, urgency.score);
//!     Ok(())
//! }
//! ```
//!
//! # Feature flags
//!
//! - `rustls` *(default)*: TLS via rustls with the platform certificate verifier.
//! - `native-tls`: TLS via the operating system's TLS library.
//! - `blocking`: the synchronous [`blocking::TypeSafeClient`].
//!
//! # Configuration
//!
//! | Option | Builder method | Environment variable | Default |
//! | --- | --- | --- | --- |
//! | API key | [`ClientBuilder::api_key`] | `TYPESAFE_API_KEY` | required |
//! | Base URL | [`ClientBuilder::base_url`] | `TYPESAFE_BASE_URL` | `https://api.typesafe.ai` |
//! | Model | [`ClientBuilder::model`] | `TYPESAFE_DEFAULT_MODEL` | `jev-latest` |
//! | Timeout | [`ClientBuilder::timeout`] | none | 10 seconds |
//! | Retries | [`ClientBuilder::retry`] | none | see [`RetryPolicy`] |
//!
//! # Logging
//!
//! The SDK emits [`tracing`] events under the `typesafe_sdk` target: one `INFO` line per
//! response and retry, and `DEBUG` lines with headers and bodies. Credential-bearing headers are
//! redacted; bodies are not. Install any subscriber to see them, for example
//! `RUST_LOG=typesafe_sdk=debug` with `tracing-subscriber`'s `EnvFilter`.

#![forbid(unsafe_code)]
#![warn(missing_docs, rust_2018_idioms, unreachable_pub)]
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod constants;

mod client;
mod config;
mod error;
mod logging;
mod question;
mod response;
mod retry;
mod transport;

#[cfg(feature = "blocking")]
#[cfg_attr(docsrs, doc(cfg(feature = "blocking")))]
pub mod blocking;

pub use client::{ClientBuilder, ListModelsRequest, Models, SystemOneRequest, TypeSafeClient};
pub use error::{
    ApiError, ApiErrorKind, ConnectionError, Error, ResponseBody, ResponseValidationError, Result, TimeoutError,
};
pub use question::{Choice, Noul, NoulCriteria, Question, Score};
pub use response::{
    Answer, ChoiceAnswer, ListModelsResponse, ModelMetadata, NoulAnswer, RawResponse, ResponseMeta, ScoreAnswer,
    SystemOneResponse, Usage,
};
pub use retry::{RetryPolicy, RetryPredicate};

/// Re-exported for building structured `state`, `instructions`, and `criteria` values.
pub use serde_json::{Map, Value, json};

/// The SDK version, as reported in the `X-TypeSafe-SDK` header.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
