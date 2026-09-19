//! Configuration resolution from explicit options and environment variables.

use std::fmt;
use std::time::Duration;

use reqwest::header::HeaderMap;

use crate::constants::{
    API_KEY_ENV, BASE_URL_ENV, DEFAULT_BASE_URL, DEFAULT_MODEL, DEFAULT_MODEL_ENV, DEFAULT_TIMEOUT,
};
use crate::error::{Error, Result};

/// Reject zero timeouts; `Duration` is already non-negative and finite.
pub(crate) fn validate_timeout(timeout: Duration) -> Result<Duration> {
    if timeout.is_zero() {
        return Err(Error::Config("timeout must be a positive, finite number of seconds.".into()));
    }
    Ok(timeout)
}

/// Explicit values take precedence over environment variables; empty or whitespace-only
/// environment values are ignored.
fn resolve_env(value: Option<String>, env: &str) -> Option<String> {
    value.or_else(|| std::env::var(env).ok().map(|raw| raw.trim().to_string()).filter(|raw| !raw.is_empty()))
}

#[derive(Clone)]
pub(crate) struct Config {
    pub(crate) api_key: String,
    pub(crate) base_url: String,
    pub(crate) default_model: String,
    pub(crate) timeout: Duration,
    pub(crate) default_headers: HeaderMap,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("base_url", &self.base_url)
            .field("default_model", &self.default_model)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl Config {
    pub(crate) fn resolve(
        api_key: Option<String>,
        base_url: Option<String>,
        default_model: Option<String>,
        timeout: Option<Duration>,
        default_headers: HeaderMap,
    ) -> Result<Self> {
        let api_key = resolve_env(api_key, API_KEY_ENV).ok_or_else(|| {
            Error::Config(format!(
                "No API key was provided. Pass api_key or set the {API_KEY_ENV} environment variable."
            ))
        })?;
        let base_url = resolve_env(base_url, BASE_URL_ENV).unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let default_model = resolve_env(default_model, DEFAULT_MODEL_ENV).unwrap_or_else(|| DEFAULT_MODEL.to_string());
        Ok(Config {
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
            default_model,
            timeout: validate_timeout(timeout.unwrap_or(DEFAULT_TIMEOUT))?,
            default_headers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_values_win_and_base_url_is_trimmed() {
        let config = Config::resolve(
            Some("key".into()),
            Some("https://example.test/".into()),
            Some("m".into()),
            Some(Duration::from_secs(3)),
            HeaderMap::new(),
        )
        .unwrap();
        assert_eq!(config.api_key, "key");
        assert_eq!(config.base_url, "https://example.test");
        assert_eq!(config.default_model, "m");
        assert_eq!(config.timeout, Duration::from_secs(3));
        assert!(!format!("{config:?}").contains("key"));
    }

    #[test]
    fn defaults_apply() {
        let config = Config::resolve(Some("key".into()), None, None, None, HeaderMap::new()).unwrap();
        assert!(config.base_url == DEFAULT_BASE_URL || std::env::var(BASE_URL_ENV).is_ok());
        assert!(config.default_model == DEFAULT_MODEL || std::env::var(DEFAULT_MODEL_ENV).is_ok());
        assert_eq!(config.timeout, DEFAULT_TIMEOUT);
    }

    #[test]
    fn zero_timeout_is_rejected() {
        let error =
            Config::resolve(Some("key".into()), None, None, Some(Duration::ZERO), HeaderMap::new()).unwrap_err();
        assert!(matches!(error, Error::Config(_)));
        assert!(error.to_string().contains("timeout must be a positive"));
    }
}
