//! `tracing` output with credential redaction.
//!
//! The SDK emits events under the `typesafe_sdk` target and never installs a subscriber. To see
//! them, install one in your application, for example with `tracing-subscriber` and
//! `RUST_LOG=typesafe_sdk=debug`. Secret headers are redacted; request and response bodies are
//! logged at `DEBUG` unredacted.

use reqwest::header::HeaderMap;

use crate::constants::SECRET_HEADERS;

pub(crate) const TARGET: &str = "typesafe_sdk";

fn is_secret(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    SECRET_HEADERS.contains(&lowered.as_str()) || lowered.contains("token") || lowered.contains("secret")
}

/// Render headers for logging, replacing credential-bearing values with `***`.
pub(crate) fn redacted(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            let rendered = if is_secret(name.as_str()) {
                "***".to_string()
            } else {
                value.to_str().map(str::to_string).unwrap_or_else(|_| "<binary>".to_string())
            };
            (name.to_string(), rendered)
        })
        .collect()
}

/// Render a body for logging as text.
pub(crate) fn body_text(body: Option<&[u8]>) -> String {
    match body {
        Some(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        None => "None".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    #[test]
    fn redacts_secret_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer secret"));
        headers.insert("x-access-token", HeaderValue::from_static("tok"));
        headers.insert("x-client-secret", HeaderValue::from_static("s"));
        headers.insert("accept", HeaderValue::from_static("application/json"));
        let rendered = redacted(&headers);
        let get = |name: &str| rendered.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str());
        assert_eq!(get("authorization"), Some("***"));
        assert_eq!(get("x-access-token"), Some("***"));
        assert_eq!(get("x-client-secret"), Some("***"));
        assert_eq!(get("accept"), Some("application/json"));
    }
}
