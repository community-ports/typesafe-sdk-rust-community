//! Retry configuration and the backoff schedule built from it.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::validate_timeout;
use crate::error::{Error, Result, parse_retry_after};

/// A caller-supplied retry predicate.
pub type RetryPredicate = Arc<dyn Fn(&Error) -> bool + Send + Sync>;

/// Configuration for SDK retry behavior.
///
/// The defaults match the Python SDK: two retries after the initial attempt, exponential
/// backoff from 0.5s to 5s with 25% jitter, retries on 408, 429, and 5xx responses as well as
/// connection and timeout errors, `Retry-After` honored, and a 30 second total budget per call.
///
/// ```
/// use std::time::Duration;
/// use typesafe_sdk::{RetryPolicy, TypeSafeClient};
///
/// let policy = RetryPolicy::default()
///     .max_retries(3)
///     .timeout(Some(Duration::from_secs(10)))
///     .http_statuses([429, 500, 502, 503, 504]);
/// let client = TypeSafeClient::builder().api_key("sk-test").retry(policy).build();
/// # let _ = client;
/// ```
#[derive(Clone)]
pub struct RetryPolicy {
    max_retries: u32,
    backoff_initial: Duration,
    backoff_max: Duration,
    backoff_jitter: f64,
    http_statuses: HashSet<u16>,
    respect_retry_after: bool,
    api_connection_error: bool,
    api_timeout_error: bool,
    predicate: Option<RetryPredicate>,
    timeout: Option<Duration>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_retries: 2,
            backoff_initial: Duration::from_millis(500),
            backoff_max: Duration::from_secs(5),
            backoff_jitter: 0.25,
            http_statuses: [408, 429].into_iter().chain(500..600).collect(),
            respect_retry_after: true,
            api_connection_error: true,
            api_timeout_error: true,
            predicate: None,
            timeout: Some(Duration::from_secs(30)),
        }
    }
}

impl fmt::Debug for RetryPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut statuses: Vec<_> = self.http_statuses.iter().copied().collect();
        statuses.sort_unstable();
        f.debug_struct("RetryPolicy")
            .field("max_retries", &self.max_retries)
            .field("backoff_initial", &self.backoff_initial)
            .field("backoff_max", &self.backoff_max)
            .field("backoff_jitter", &self.backoff_jitter)
            .field("http_statuses", &statuses)
            .field("respect_retry_after", &self.respect_retry_after)
            .field("api_connection_error", &self.api_connection_error)
            .field("api_timeout_error", &self.api_timeout_error)
            .field("predicate", &self.predicate.as_ref().map(|_| "<fn>"))
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl RetryPolicy {
    /// A policy that never retries.
    pub fn none() -> Self {
        RetryPolicy::default().max_retries(0)
    }

    /// Maximum retries after the initial attempt; `0` disables retries. Default `2`.
    pub fn max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// First backoff delay, doubled each attempt up to `backoff_max`; zero disables backoff.
    /// Default 500ms.
    pub fn backoff_initial(mut self, initial: Duration) -> Self {
        self.backoff_initial = initial;
        self
    }

    /// Maximum backoff delay; zero disables backoff. Default 5s.
    pub fn backoff_max(mut self, max: Duration) -> Self {
        self.backoff_max = max;
        self
    }

    /// Fraction of each backoff delay randomly subtracted, between 0 and 1. Default `0.25`.
    pub fn backoff_jitter(mut self, jitter: f64) -> Self {
        self.backoff_jitter = jitter;
        self
    }

    /// HTTP status codes that are retried. Default `408`, `429`, and `500..600`.
    pub fn http_statuses(mut self, statuses: impl IntoIterator<Item = u16>) -> Self {
        self.http_statuses = statuses.into_iter().collect();
        self
    }

    /// Whether to honor `Retry-After` and `retry-after-ms` response headers. Default `true`.
    pub fn respect_retry_after(mut self, respect: bool) -> Self {
        self.respect_retry_after = respect;
        self
    }

    /// Whether to retry [`Error::Connection`], raised when the request cannot reach or read
    /// from the server. Default `true`.
    pub fn api_connection_error(mut self, retry: bool) -> Self {
        self.api_connection_error = retry;
        self
    }

    /// Whether to retry [`Error::Timeout`], raised when the request exceeds its timeout.
    /// Default `true`.
    pub fn api_timeout_error(mut self, retry: bool) -> Self {
        self.api_timeout_error = retry;
        self
    }

    /// An optional predicate called with the error; returning `true` triggers a retry in
    /// addition to the other rules.
    pub fn predicate(mut self, predicate: impl Fn(&Error) -> bool + Send + Sync + 'static) -> Self {
        self.predicate = Some(Arc::new(predicate));
        self
    }

    /// Total retry budget per SDK call, including the initial attempt and delays; `None`
    /// disables the limit. Default 30s.
    ///
    /// The loop stops before a retry whose delay would reach or exceed the budget, returning
    /// the last error.
    pub fn timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    /// Validate jitter and the optional budget, mirroring the Python SDK's `__post_init__`.
    pub(crate) fn validate(&self) -> Result<()> {
        if !(0.0..=1.0).contains(&self.backoff_jitter) || self.backoff_jitter.is_nan() {
            return Err(Error::Config("backoff_jitter must be between zero and one.".into()));
        }
        if let Some(timeout) = self.timeout {
            validate_timeout(timeout)?;
        }
        Ok(())
    }

    pub(crate) fn is_retryable(&self, error: &Error) -> bool {
        let builtin = match error {
            Error::Timeout(_) => self.api_timeout_error,
            Error::Connection(_) => self.api_connection_error,
            Error::Api(api) => self.http_statuses.contains(&api.status().as_u16()),
            _ => false,
        };
        builtin || self.predicate.as_ref().is_some_and(|predicate| predicate(error))
    }

    /// The delay before the retry following `attempt` (1-based count of failed attempts).
    pub(crate) fn wait(&self, attempt: u32, error: &Error) -> Duration {
        if self.respect_retry_after
            && let Error::Api(api) = error
            && let Some(delay) = parse_retry_after(api.headers())
        {
            return delay;
        }
        backoff(attempt, self.backoff_initial, self.backoff_max, self.backoff_jitter)
    }

    /// Decide what to do after a failed attempt: `Some(delay)` to retry after sleeping, or
    /// `None` to give up and return the error.
    pub(crate) fn next_delay(&self, attempt: u32, started: Instant, error: &Error) -> Option<Duration> {
        if attempt > self.max_retries || !self.is_retryable(error) {
            return None;
        }
        let delay = self.wait(attempt, error);
        if let Some(budget) = self.timeout
            && started.elapsed() + delay >= budget
        {
            return None;
        }
        Some(delay)
    }
}

/// Exponential backoff with subtractive jitter, rounded to milliseconds and capped at the
/// undithered value, matching the Python SDK's schedule.
fn backoff(attempt: u32, initial: Duration, maximum: Duration, jitter: f64) -> Duration {
    if initial.is_zero() || maximum.is_zero() {
        return Duration::ZERO;
    }
    let (initial_s, maximum_s) = (initial.as_secs_f64(), maximum.as_secs_f64());
    let exponent = attempt.saturating_sub(1);
    let exponential = if f64::from(exponent) >= maximum_s.log2() - initial_s.log2() {
        maximum_s
    } else {
        initial_s * 2f64.powi(exponent as i32)
    };
    let delay = exponential * (1.0 - fastrand::f64() * jitter);
    let rounded = (delay * 1000.0).round() / 1000.0;
    Duration::from_secs_f64(rounded.min(exponential).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{ApiError, ResponseBody, TimeoutError};
    use reqwest::StatusCode;
    use reqwest::header::{HeaderMap, HeaderValue};

    fn api_error(status: u16, headers: HeaderMap) -> Error {
        ApiError::new(StatusCode::from_u16(status).unwrap(), ResponseBody::Empty, headers, None).into()
    }

    #[test]
    fn backoff_without_jitter_doubles_and_caps() {
        let s = |ms| Duration::from_millis(ms);
        assert_eq!(backoff(1, s(500), s(5000), 0.0), s(500));
        assert_eq!(backoff(2, s(500), s(5000), 0.0), s(1000));
        assert_eq!(backoff(3, s(500), s(5000), 0.0), s(2000));
        assert_eq!(backoff(4, s(500), s(5000), 0.0), s(4000));
        assert_eq!(backoff(5, s(500), s(5000), 0.0), s(5000));
        assert_eq!(backoff(50, s(500), s(5000), 0.0), s(5000));
        assert_eq!(backoff(3, Duration::ZERO, s(5000), 0.0), Duration::ZERO);
        assert_eq!(backoff(3, s(500), Duration::ZERO, 0.0), Duration::ZERO);
    }

    #[test]
    fn backoff_jitter_only_subtracts() {
        for _ in 0..200 {
            let delay = backoff(3, Duration::from_millis(500), Duration::from_secs(5), 0.25);
            assert!(delay <= Duration::from_secs(2), "{delay:?}");
            assert!(delay >= Duration::from_millis(1500), "{delay:?}");
        }
    }

    #[test]
    fn retryable_rules() {
        let policy = RetryPolicy::default();
        assert!(policy.is_retryable(&api_error(500, HeaderMap::new())));
        assert!(policy.is_retryable(&api_error(429, HeaderMap::new())));
        assert!(policy.is_retryable(&api_error(408, HeaderMap::new())));
        assert!(!policy.is_retryable(&api_error(400, HeaderMap::new())));
        assert!(policy.is_retryable(&Error::Timeout(TimeoutError::new(Duration::from_secs(1)))));
        assert!(!policy.is_retryable(&Error::Config("x".into())));

        let strict = RetryPolicy::default().api_timeout_error(false).http_statuses([503]);
        assert!(!strict.is_retryable(&Error::Timeout(TimeoutError::new(Duration::from_secs(1)))));
        assert!(!strict.is_retryable(&api_error(500, HeaderMap::new())));
        assert!(strict.is_retryable(&api_error(503, HeaderMap::new())));

        let custom = RetryPolicy::default().predicate(|error| matches!(error, Error::Config(_)));
        assert!(custom.is_retryable(&Error::Config("x".into())));
    }

    #[test]
    fn wait_honors_retry_after() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after-ms", HeaderValue::from_static("1500"));
        let error = api_error(429, headers);
        assert_eq!(RetryPolicy::default().wait(1, &error), Duration::from_millis(1500));
        let ignored = RetryPolicy::default().respect_retry_after(false).backoff_jitter(0.0).wait(1, &error);
        assert_eq!(ignored, Duration::from_millis(500));
    }

    #[test]
    fn next_delay_respects_attempts_and_budget() {
        let policy = RetryPolicy::default().backoff_jitter(0.0);
        let error = api_error(500, HeaderMap::new());
        let started = Instant::now();
        assert_eq!(policy.next_delay(1, started, &error), Some(Duration::from_millis(500)));
        assert_eq!(policy.next_delay(2, started, &error), Some(Duration::from_secs(1)));
        assert_eq!(policy.next_delay(3, started, &error), None);

        let tight = RetryPolicy::default().backoff_jitter(0.0).timeout(Some(Duration::from_millis(400)));
        assert_eq!(tight.next_delay(1, started, &error), None);
        let none = RetryPolicy::none();
        assert_eq!(none.next_delay(1, started, &error), None);
    }

    #[test]
    fn validation() {
        assert!(RetryPolicy::default().validate().is_ok());
        assert!(RetryPolicy::default().backoff_jitter(1.5).validate().is_err());
        assert!(RetryPolicy::default().backoff_jitter(-0.1).validate().is_err());
        assert!(RetryPolicy::default().backoff_jitter(f64::NAN).validate().is_err());
        assert!(RetryPolicy::default().timeout(Some(Duration::ZERO)).validate().is_err());
        assert!(RetryPolicy::default().timeout(None).validate().is_ok());
    }

    #[test]
    fn debug_hides_predicate_body() {
        let text = format!("{:?}", RetryPolicy::default().predicate(|_| true));
        assert!(text.contains("predicate: Some(\"<fn>\")"), "{text}");
    }
}
