//! Public environment-variable names and client defaults, plus internal protocol constants.

use std::time::Duration;

/// Environment variable for the API key.
pub const API_KEY_ENV: &str = "TYPESAFE_API_KEY";

/// Environment variable for the API base URL.
pub const BASE_URL_ENV: &str = "TYPESAFE_BASE_URL";

/// Environment variable for the default model.
pub const DEFAULT_MODEL_ENV: &str = "TYPESAFE_DEFAULT_MODEL";

/// Default API base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";

/// Default model name.
pub const DEFAULT_MODEL: &str = "jev-latest";

/// Default timeout for each HTTP operation.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

// --- Internal protocol constants -------------------------------------------------------------

pub(crate) const MAX_ERROR_BODY_LENGTH: usize = 200;

pub(crate) const SYSTEM_ONE_PATH: &str = "/v1/systemone";
pub(crate) const MODELS_PATH: &str = "/v1/models";

pub(crate) const SDK_NAME: &str = "typesafeai-sdk-community";
pub(crate) const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) const JSON_CONTENT_TYPE: &str = "application/json";

pub(crate) const SDK_HEADER: &str = "x-typesafe-sdk";
pub(crate) const RUNTIME_HEADER: &str = "x-typesafe-runtime";
pub(crate) const RETRY_COUNT_HEADER: &str = "x-typesafe-retry-count";
pub(crate) const REQUEST_ID_HEADER: &str = "x-typesafe-request-id";
pub(crate) const RETRY_AFTER_HEADER: &str = "retry-after";
pub(crate) const RETRY_AFTER_MS_HEADER: &str = "retry-after-ms";

/// Header names whose values are always redacted from log output.
pub(crate) const SECRET_HEADERS: &[&str] =
    &["authorization", "proxy-authorization", "x-api-key", "api-key", "cookie", "set-cookie"];

/// The `X-TypeSafe-Runtime` value, e.g. `rust/1.96.0 (linux; x86_64)` or `rust/1.96.0 (macos; aarch64)`.
/// The OS and architecture are read at runtime, mirroring the Python SDK's `python/<version> (<platform>; <machine>)`.
pub(crate) fn runtime() -> String {
    format!("rust/{} ({}; {})", env!("TYPESAFE_SDK_RUSTC_VERSION"), std::env::consts::OS, std::env::consts::ARCH)
}

/// The `User-Agent` / `X-TypeSafe-SDK` value, e.g. `typesafeai-sdk-community/0.1.0`.
pub(crate) fn sdk_identifier() -> String {
    format!("{SDK_NAME}/{SDK_VERSION}")
}
