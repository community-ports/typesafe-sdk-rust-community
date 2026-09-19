//! A response cache for repeated judgments on unchanged evidence.
//!
//! A System One request is deterministic in its inputs (model, state, questions), so the same
//! request can reuse its earlier answer instead of spending tokens again: batch jobs that
//! re-run over mostly unchanged data, retried pipelines, tests, and fan-out code that asks the
//! same question from several places.
//!
//! ```
//! use std::time::Duration;
//! use typesafeai_sdk_community::cache::Cache;
//! use typesafeai_sdk_community::TypeSafeClient;
//!
//! let client = TypeSafeClient::builder()
//!     .api_key("sk-...")
//!     .cache(Cache::in_memory(10_000).ttl(Duration::from_secs(3600)))
//!     .build();
//! # let _ = client;
//! ```
//!
//! Then on any request: `.cache_scope("tenant-42")` adds a dimension to the key without
//! changing the request, `.refresh()` skips the read but stores the result, and `.no_cache()`
//! bypasses the cache entirely. Cached responses have `meta.from_cache == true` and keep the
//! original request ID. Errors are never cached.
//!
//! The key is customizable at two levels: a [`Cache::key`] function replaces the default
//! (a 128-bit hash of method, URL, canonical body, and scope), and [`CacheStore`] is a trait
//! so the entries can live in Redis, on disk, or anywhere else; [`InMemoryStore`] is the
//! default.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

use crate::transport::{HttpResponse, PreparedRequest};

/// Everything the default key is computed from, offered to a custom key function.
#[derive(Debug)]
pub struct KeyInput<'a> {
    /// The HTTP method.
    pub method: &'a str,
    /// The absolute URL.
    pub url: &'a str,
    /// The JSON request body (canonical: object keys are sorted).
    pub body: &'a [u8],
    /// The scope set with `.cache_scope(...)`, if any.
    pub scope: Option<&'a str>,
}

/// A cached successful response.
#[derive(Clone, Debug)]
pub struct CachedResponse {
    /// The HTTP status (always successful).
    pub status: u16,
    /// Response headers, as name/value pairs.
    pub headers: Vec<(String, String)>,
    /// The response body.
    pub body: Vec<u8>,
}

impl CachedResponse {
    pub(crate) fn from_http(response: &HttpResponse) -> Self {
        CachedResponse {
            status: response.status.as_u16(),
            headers: response
                .headers
                .iter()
                .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_string())))
                .collect(),
            body: response.body.clone(),
        }
    }

    pub(crate) fn into_http(self) -> Option<HttpResponse> {
        let mut headers = HeaderMap::new();
        for (name, value) in &self.headers {
            if let (Ok(name), Ok(value)) = (HeaderName::from_bytes(name.as_bytes()), HeaderValue::from_str(value)) {
                headers.append(name, value);
            }
        }
        Some(HttpResponse { status: StatusCode::from_u16(self.status).ok()?, headers, body: self.body })
    }
}

/// Where cached responses live. Implement for Redis, disk, or a shared process cache.
pub trait CacheStore: Send + Sync + 'static {
    /// A stored response, if present and not expired.
    fn get(&self, key: &str) -> Option<CachedResponse>;
    /// Store a response; `ttl` is the cache's configured time to live, if any.
    fn set(&self, key: String, response: CachedResponse, ttl: Option<Duration>);
    /// Remove one entry.
    fn remove(&self, key: &str);
    /// Remove every entry.
    fn clear(&self);
}

struct Entry {
    response: CachedResponse,
    expires: Option<Instant>,
    used: u64,
}

/// An in-process store with a capacity (least recently used entries are evicted first) and
/// per-entry expiry.
pub struct InMemoryStore {
    capacity: usize,
    entries: Mutex<HashMap<String, Entry>>,
    clock: AtomicU64,
}

impl InMemoryStore {
    /// A store holding at most `capacity` responses.
    pub fn new(capacity: usize) -> Self {
        InMemoryStore { capacity: capacity.max(1), entries: Mutex::new(HashMap::new()), clock: AtomicU64::new(0) }
    }

    /// How many entries are stored, including ones that may have expired.
    pub fn len(&self) -> usize {
        self.entries.lock().expect("cache lock").len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl CacheStore for InMemoryStore {
    fn get(&self, key: &str) -> Option<CachedResponse> {
        let mut entries = self.entries.lock().expect("cache lock");
        let expired = entries.get(key).is_some_and(|e| e.expires.is_some_and(|at| at <= Instant::now()));
        if expired {
            entries.remove(key);
            return None;
        }
        let entry = entries.get_mut(key)?;
        entry.used = self.clock.fetch_add(1, Ordering::Relaxed);
        Some(entry.response.clone())
    }

    fn set(&self, key: String, response: CachedResponse, ttl: Option<Duration>) {
        let mut entries = self.entries.lock().expect("cache lock");
        if entries.len() >= self.capacity && !entries.contains_key(&key) {
            let now = Instant::now();
            entries.retain(|_, e| e.expires.is_none_or(|at| at > now));
            if entries.len() >= self.capacity
                && let Some(oldest) = entries.iter().min_by_key(|(_, e)| e.used).map(|(k, _)| k.clone())
            {
                entries.remove(&oldest);
            }
        }
        let used = self.clock.fetch_add(1, Ordering::Relaxed);
        entries.insert(key, Entry { response, expires: ttl.map(|ttl| Instant::now() + ttl), used });
    }

    fn remove(&self, key: &str) {
        self.entries.lock().expect("cache lock").remove(key);
    }

    fn clear(&self) {
        self.entries.lock().expect("cache lock").clear();
    }
}

/// How a request interacts with the cache.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CacheMode {
    /// Read and write.
    #[default]
    Use,
    /// Skip the read; store the fresh response.
    Refresh,
    /// Neither read nor write.
    Bypass,
}

/// Hit and miss counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Requests answered from the cache.
    pub hits: u64,
    /// Requests that went to the API.
    pub misses: u64,
    /// Responses stored.
    pub stores: u64,
}

type KeyFn = Arc<dyn Fn(&KeyInput<'_>) -> String + Send + Sync>;

/// A configured cache: a store, an optional time to live, and the key function.
#[derive(Clone)]
pub struct Cache {
    store: Arc<dyn CacheStore>,
    ttl: Option<Duration>,
    key_fn: Option<KeyFn>,
    hits: Arc<AtomicU64>,
    misses: Arc<AtomicU64>,
    stores: Arc<AtomicU64>,
}

impl fmt::Debug for Cache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cache")
            .field("ttl", &self.ttl)
            .field("custom_key", &self.key_fn.is_some())
            .field("stats", &self.stats())
            .finish()
    }
}

impl Cache {
    /// A cache over any store, with no expiry.
    pub fn new(store: impl CacheStore) -> Self {
        Self::with_store(Arc::new(store))
    }

    /// A cache over a shared store.
    pub fn with_store(store: Arc<dyn CacheStore>) -> Self {
        Cache { store, ttl: None, key_fn: None, hits: Arc::default(), misses: Arc::default(), stores: Arc::default() }
    }

    /// An in-process cache holding at most `capacity` responses.
    pub fn in_memory(capacity: usize) -> Self {
        Self::new(InMemoryStore::new(capacity))
    }

    /// Expire entries this long after they are stored.
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// Replace the key function. It receives the method, URL, canonical body, and scope, and
    /// returns the store key. Include everything that should distinguish entries.
    pub fn key(mut self, f: impl Fn(&KeyInput<'_>) -> String + Send + Sync + 'static) -> Self {
        self.key_fn = Some(Arc::new(f));
        self
    }

    /// The key a request would use, so entries can be invalidated by request.
    pub fn key_for(&self, input: &KeyInput<'_>) -> String {
        match &self.key_fn {
            Some(f) => f(input),
            None => default_key(input),
        }
    }

    /// The underlying store.
    pub fn store(&self) -> &dyn CacheStore {
        self.store.as_ref()
    }

    /// Remove one entry by key.
    pub fn invalidate(&self, key: &str) {
        self.store.remove(key);
    }

    /// Remove every entry.
    pub fn clear(&self) {
        self.store.clear();
    }

    /// Hit, miss, and store counts since the cache was created.
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            stores: self.stores.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn request_key(&self, request: &PreparedRequest, scope: Option<&str>) -> String {
        self.key_for(&KeyInput {
            method: request.method.as_str(),
            url: &request.url,
            body: request.body.as_deref().unwrap_or(&[]),
            scope,
        })
    }

    pub(crate) fn lookup(&self, key: &str) -> Option<HttpResponse> {
        match self.store.get(key).and_then(CachedResponse::into_http) {
            Some(response) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(target: crate::logging::TARGET, "cache hit {key}");
                Some(response)
            }
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    pub(crate) fn store_response(&self, key: String, response: &HttpResponse) {
        if response.status.is_success() {
            self.stores.fetch_add(1, Ordering::Relaxed);
            self.store.set(key, CachedResponse::from_http(response), self.ttl);
        }
    }
}

/// The default key: `method url scope` and a 128-bit FNV-1a hash of the body, rendered as hex.
/// Stable across processes and SDK versions, so persistent stores stay valid.
pub fn default_key(input: &KeyInput<'_>) -> String {
    let (a, b) = fnv128(input.body);
    let scope = input.scope.unwrap_or("");
    let (sa, sb) = fnv128(scope.as_bytes());
    format!("typesafeai:{}:{}:{a:016x}{b:016x}:{sa:08x}{sb:08x}", input.method, input.url)
}

/// Two independent 64-bit FNV-1a hashes (different offset bases), for a 128-bit key.
fn fnv128(bytes: &[u8]) -> (u64, u64) {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut a: u64 = 0xcbf2_9ce4_8422_2325;
    let mut b: u64 = 0x84222325_cbf29ce4;
    for &byte in bytes {
        a ^= u64::from(byte);
        a = a.wrapping_mul(PRIME);
        b ^= u64::from(byte.rotate_left(3));
        b = b.wrapping_mul(PRIME);
    }
    (a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(body: &str) -> CachedResponse {
        CachedResponse { status: 200, headers: vec![("x-typesafe-request-id".into(), "r".into())], body: body.into() }
    }

    #[test]
    fn keys_are_stable_and_distinct() {
        let a = default_key(&KeyInput { method: "POST", url: "u", body: b"{\"a\":1}", scope: None });
        let same = default_key(&KeyInput { method: "POST", url: "u", body: b"{\"a\":1}", scope: None });
        let other_body = default_key(&KeyInput { method: "POST", url: "u", body: b"{\"a\":2}", scope: None });
        let scoped = default_key(&KeyInput { method: "POST", url: "u", body: b"{\"a\":1}", scope: Some("t") });
        assert_eq!(a, same);
        assert_ne!(a, other_body);
        assert_ne!(a, scoped);
        assert!(a.starts_with("typesafeai:POST:u:"));
    }

    #[test]
    fn in_memory_store_evicts_and_expires() {
        let store = InMemoryStore::new(2);
        store.set("a".into(), response("a"), None);
        store.set("b".into(), response("b"), None);
        assert!(store.get("a").is_some()); // a is now most recently used
        store.set("c".into(), response("c"), None);
        assert!(store.get("b").is_none(), "b was least recently used");
        assert!(store.get("a").is_some());
        assert!(store.get("c").is_some());
        assert_eq!(store.len(), 2);

        store.set("t".into(), response("t"), Some(Duration::from_millis(1)));
        std::thread::sleep(Duration::from_millis(5));
        assert!(store.get("t").is_none());
        store.remove("a");
        assert!(store.get("a").is_none());
        store.clear();
        assert!(store.is_empty());
    }

    #[test]
    fn cache_round_trip_and_stats() {
        let cache = Cache::in_memory(10).ttl(Duration::from_secs(60));
        let http = response("{\"model\":\"m\"}").into_http().unwrap();
        assert_eq!(http.status, StatusCode::OK);
        assert!(cache.lookup("k").is_none());
        cache.store_response("k".into(), &http);
        let hit = cache.lookup("k").unwrap();
        assert_eq!(hit.body, http.body);
        assert_eq!(hit.headers.get("x-typesafe-request-id").unwrap(), "r");
        assert_eq!(cache.stats(), CacheStats { hits: 1, misses: 1, stores: 1 });

        let failed = HttpResponse { status: StatusCode::BAD_GATEWAY, headers: HeaderMap::new(), body: Vec::new() };
        cache.store_response("bad".into(), &failed);
        assert!(cache.lookup("bad").is_none());
        cache.invalidate("k");
        assert!(cache.lookup("k").is_none());

        let custom = Cache::in_memory(1).key(|input| format!("only-scope:{}", input.scope.unwrap_or("none")));
        assert_eq!(
            custom.key_for(&KeyInput { method: "POST", url: "u", body: b"x", scope: Some("s") }),
            "only-scope:s"
        );
    }
}
