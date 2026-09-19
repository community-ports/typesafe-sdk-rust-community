# TypeSafe AI Rust SDK (community)

A community-built Rust SDK for the [TypeSafe AI](https://typesafe.ai) API. It is a port of the
official [Python SDK](https://github.com/typesafe-ai/typesafe-sdk-python) with the same
primitives, wire format, retry behavior, and error semantics, in idiomatic Rust.

TypeSafe's **System One** models (Jev is the flagship) return fast, typed judgments
(probabilities, selections, and scores) that your code can consume directly instead of
generated text. Learn what TypeSafe is and how to design questions in the
[TypeSafe docs](https://docs.typesafe.ai/).

> This is an independent community project and is not maintained by TypeSafe AI.

## Quickstart

```toml
[dependencies]
typesafe-sdk = "0.1"
tokio = { version = "1", features = ["full"] }
```

Set `TYPESAFE_API_KEY` in your environment, then:

```rust
use typesafe_sdk::{Choice, Noul, Score, TypeSafeClient, json};

#[tokio::main]
async fn main() -> typesafe_sdk::Result<()> {
    let client = TypeSafeClient::new()?;

    let response = client
        .system_one(json!({"document": "I was charged twice. Please help."}))
        .question("billing", Noul::new("Is this message about billing?"))
        .question(
            "tone",
            Choice::new("What is the tone of this message?")
                .option("angry", "An upset or hostile message")
                .option("calm", "A neutral or polite message"),
        )
        .question("urgency", Score::new("How urgent is this?", ["Can wait", "This week", "Today"]))
        .send()
        .await?;

    let billing = response.noul("billing").unwrap();
    let tone = response.choice("tone").unwrap();
    let urgency = response.score("urgency").unwrap();
    println!("billing={:.2} tone={} ({:.2}) urgency={:.1}", billing.noul, tone.choice, tone.confidence, urgency.score);
    Ok(())
}
```

The crate is `typesafe-sdk` and imports as `typesafe_sdk`, mirroring the Python package and
module names.

## Questions

| Need | Primitive | Answer |
| --- | --- | --- |
| Whether a condition holds | `Noul` | `NoulAnswer { noul: f64 }`, the probability of "yes" |
| One of a defined set | `Choice` | `ChoiceAnswer { choice, confidence, probabilities }` |
| Degree along an ordered rubric | `Score` | `ScoreAnswer { score, confidence, legend, probabilities }` |

Instructions and criteria accept plain strings or structured JSON via the re-exported `json!`
macro. `Question::Custom` sends an arbitrary JSON object verbatim for question types this SDK
does not model yet.

```rust
use typesafe_sdk::{Choice, Noul, Score, json};

let spam = Noul::new("Is this message spam?")
    .when_true("Unsolicited advertising")
    .when_false("A legitimate conversation");

let tone = Choice::new("What is the tone?").labels(["angry", "calm", "excited"]);

let urgency = Score::new(json!({"task": "How urgent is `message`?"}), ["Can wait", "This week", "Today"]);
```

## Responses

`SystemOneResponse` exposes `model`, `usage`, all `answers` keyed by question name, and typed
accessors: `noul(name)`, `choice(name)`, `score(name)`, plus `nouls()`, `choices()`, and
`scores()` iterators. `request_id()` returns the `x-typesafe-request-id` header and `meta` holds
the HTTP status and headers.

For a custom shape, decode the body into your own `serde` types with `send_as::<T>()`, or get
the undecoded bytes with `send_raw()`:

```rust
#[derive(serde::Deserialize)]
struct Answers { billing: typesafe_sdk::NoulAnswer }
#[derive(serde::Deserialize)]
struct Mine { model: String, answers: Answers }

let mine: Mine = client.system_one("...").question("billing", Noul::new("Billing?")).send_as().await?;
```

## Configuration

| Option | Builder method | Environment variable | Default |
| --- | --- | --- | --- |
| API key | `api_key` | `TYPESAFE_API_KEY` | required |
| Base URL | `base_url` | `TYPESAFE_BASE_URL` | `https://api.typesafe.ai` |
| Model | `model` | `TYPESAFE_DEFAULT_MODEL` | `jev-latest` |
| Timeout | `timeout` | none | 10 seconds |
| Retries | `retry` | none | see below |
| Headers | `header` / `headers` | none | none |
| HTTP client | `http_client` | none | a default `reqwest::Client` |

Explicit options win over environment variables; empty or whitespace-only environment values
are ignored. Every option except the API key can also be overridden per call on the request
builder (`.model(..)`, `.timeout(..)`, `.retry(..)`, `.header(..)`, `.extra_body(..)`).

```rust
use std::time::Duration;
use typesafe_sdk::{RetryPolicy, TypeSafeClient};

let client = TypeSafeClient::builder()
    .api_key("sk-...")
    .model("jev-latest")
    .timeout(Duration::from_secs(20))
    .retry(RetryPolicy::default().max_retries(3))
    .header("x-team", "billing")
    .build()?;
```

## Retries

Requests are retried on connection errors, timeouts, and `408`, `429`, and `5xx` responses:
two retries after the initial attempt, exponential backoff from 0.5s to 5s with 25% jitter,
`Retry-After` / `retry-after-ms` honored, and a 30 second total budget per call. Retried
attempts carry an `X-TypeSafe-Retry-Count` header. Tune or disable this with `RetryPolicy`:

```rust
use typesafe_sdk::{Error, RetryPolicy};

let policy = RetryPolicy::default()
    .max_retries(5)
    .http_statuses([429, 502, 503, 504])
    .timeout(None)
    .predicate(|error| matches!(error, Error::Api(api) if api.message().contains("overloaded")));

let none = RetryPolicy::none();
```

## Errors

Every operation returns `typesafe_sdk::Result<T>`; the error is one enum:

| Variant | Meaning |
| --- | --- |
| `Error::Config` | Missing API key, invalid timeout, retry policy, or header |
| `Error::InvalidRequest` | No questions, an empty score rubric, or an unencodable body |
| `Error::Api(ApiError)` | Unsuccessful HTTP status after retries; `kind()` classifies it (`BadRequest`, `Authentication`, `PermissionDenied`, `NotFound`, `UnprocessableEntity`, `RateLimit`, `InternalServer`, `Other`), and `retry_after()`, `request_id()`, `body()`, `headers()` expose the details |
| `Error::ResponseValidation` | A `2xx` body missing required data; `field_path()` names the offending field, e.g. `answers.tone.confidence` |
| `Error::Connection` | The request never got an HTTP response |
| `Error::Timeout` | The request exceeded its timeout |

```rust
match client.system_one("...").question("q", Noul::new("?")).send().await {
    Ok(response) => { /* ... */ }
    Err(Error::Api(api)) if api.kind() == ApiErrorKind::RateLimit => {
        eprintln!("rate limited; retry after {:?}", api.retry_after());
    }
    Err(error) => eprintln!("request failed: {error}"),
}
```

## Blocking client

Enable the `blocking` feature for a synchronous client with the same API:

```toml
typesafe-sdk = { version = "0.1", features = ["blocking"] }
```

```rust
use typesafe_sdk::blocking::TypeSafeClient;
use typesafe_sdk::Noul;

let client = TypeSafeClient::new()?;
let response = client.system_one("...").question("billing", Noul::new("Billing?")).send()?;
```

Like `reqwest::blocking`, it must not be used from inside an async runtime.

## Logging

The SDK emits `tracing` events under the `typesafe_sdk` target: an `INFO` line per response
and retry, and `DEBUG` lines with headers and bodies. Credential-bearing headers are redacted;
bodies are not. Install any subscriber to see them, for example:

```sh
RUST_LOG=typesafe_sdk=debug cargo run --example models
```

## Feature flags

| Feature | Default | Effect |
| --- | --- | --- |
| `rustls` | yes | TLS via rustls with the platform certificate verifier |
| `native-tls` | no | TLS via the operating system's TLS library |
| `blocking` | no | The synchronous `blocking::TypeSafeClient` |

## Development

```sh
cargo test --all-features          # offline; live tests self-skip without TYPESAFE_API_KEY
TYPESAFE_API_KEY=... cargo test --all-features --test live
cargo clippy --all-features --all-targets -- -D warnings
cargo fmt --all
cargo run --example basic
```

Minimum supported Rust version: 1.88.

## Related

- [TypeSafe docs](https://docs.typesafe.ai/): concepts, primitives, cookbooks, HTTP API
- [typesafe-sdk-python](https://github.com/typesafe-ai/typesafe-sdk-python): the official Python SDK this crate mirrors

## License

MIT. See [LICENSE](LICENSE).
