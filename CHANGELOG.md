# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.3.1] - 2026-09-19

### Fixed

- README install snippets on crates.io named `0.1`; they now track the current minor version,
  and the release workflow refuses a tag whose README is stale.

## [0.3.0] - 2026-09-19

Typed routing, composite scoring, and a testing story.

### Added

- `#[derive(Route)]`: an enum whose variants are selected by one choice question and whose
  named fields are questions of their own, all sent in one request. `client.route::<T>(state)`
  returns the variant; `send_full()` returns `Routed<T>` with the routing `ChoiceAnswer` and
  the full response. Available on the blocking client.
- `#[derive(Composite)]` and the `composite` module: `#[weight(w)]` fields (with `label = ...`,
  `invert`, and `name = ...`) combine into `composite()`, `composite_with(&Weights)`, and
  `breakdown()`. `Signal` and `LabelSignal` read answers as 0..1 values; `Weights` is
  serializable.
- The `test-util` feature and `testing` module: `MockTransport` with scripted `MockResponse`s
  (answers, typed answers, errors, rate limits, timeouts, disconnects), request recording,
  `mock.client()`, and `Recorder` / `Cassette` record-and-replay.
- `#[questions(mock)]` and `#[route(..., mock)]` generate typed fixture builders
  (`Triage::mock().tone(Tone::Angry).build()`).
- The `transport` module is public: `Transport` / `BlockingTransport` traits, `HttpRequest`,
  `HttpResponse`, and the default `ReqwestTransport`s. `ClientBuilder::transport` plugs in any
  implementation.
- `ChoiceOf` / `ScoreOf` helper traits and `Routed<T>`.

### Changed

- `TypeSafeClient::http_client()` now returns `Option<&reqwest::Client>` (a custom transport
  has none); `transport()` returns the transport in use.
- `ConnectionError::new` builds a connection error without a `reqwest` source, for custom
  transports.

## [0.2.0] - 2026-09-19

The community edition's first additions beyond the official SDK surface: typed questions and
answers, and decision helpers.

### Added

- `#[derive(ChoiceLabels)]` and `#[derive(ScoreLevels)]` make a unit enum the labels of a
  choice question or the rubric of a score question, with `rename_all`, `label`, and
  `describe` attributes. `Choice::of::<T>` and `Score::of::<T>` build questions from them.
- `#[derive(Questions)]` makes a struct a whole question set: every field is one question and
  the struct is filled from the response. Field types pick the shape (`NoulAnswer`, `bool`,
  `f64`, `ChoiceAnswer`, `TypedChoice<T>`, `T`, `ScoreAnswer`, `TypedScore<T>`, `Answer`, or
  `Option` of any); a mismatched question kind is a compile error.
- `client.ask::<T>(state)` on both clients sends a question set in one request and returns
  `T`, or `Answered<T>` with the full response via `send_full()`.
- `TypedChoice<T>` and `TypedScore<T>` with typed probabilities, `probability`, `top`,
  `margin`, `probability_at_least`/`at_most`, `normalized`, and `gate`.
- `SystemOneResponse::parse::<T>()`, `choice_as::<T>()`, `score_as::<T>()`, and `get::<T>()`.
- The `decision` module: `Bands`/`Decision` for three-way noul decisions, `Gate`/`Outcome` for
  accept/review/reject on confidence, and `margin`, `top`, `entropy`, `normalized_entropy`,
  `probability_at_least`, `std_dev`, `normalized`, and `certainty` on the answer types.
- `Error::Answer(AnswerError)` for missing, mismatched, or unknown-label answers.
- The `derive` feature (on by default) and the `typesafeai-sdk-community-macros` crate.

## [0.1.0] - 2026-09-19

Initial port of [typesafe-sdk-python](https://github.com/typesafe-ai/typesafe-sdk-python) 0.7.0.

### Added

- Async `TypeSafeClient` on `reqwest`/`tokio`, and a `blocking::TypeSafeClient` behind the
  `blocking` feature.
- `Noul`, `Choice`, and `Score` question builders plus `Question::Custom` for forward
  compatibility.
- `POST /v1/systemone` via `client.system_one(state)` with per-call `model`, `retry`,
  `timeout`, `header`, and `extra_body` overrides; `send`, `send_as::<T>`, and `send_raw`.
- `GET /v1/models` via `client.models().list()`.
- `SystemOneResponse` with typed accessors, integer-keyed score legends and probabilities,
  request IDs, and forward-compatible dropping of unknown answer types.
- `RetryPolicy` matching the Python SDK: exponential backoff with jitter, `Retry-After` /
  `retry-after-ms`, total budget, retryable status set, connection/timeout toggles, and a
  custom predicate; `X-TypeSafe-Retry-Count` on retried attempts.
- `Error` enum with `ApiError` (classified by status, with `retry_after`), `ResponseValidationError`
  (with dotted `field_path`), `ConnectionError`, and `TimeoutError`.
- Configuration from `TYPESAFE_API_KEY`, `TYPESAFE_BASE_URL`, and `TYPESAFE_DEFAULT_MODEL`.
- `tracing` output under the `typesafeai_sdk_community` target with secret-header redaction.
- `X-TypeSafe-SDK`, `X-TypeSafe-Runtime` (runtime-detected OS and architecture), and
  `User-Agent` identification headers.

[Unreleased]: https://github.com/community-ports/typesafeai-sdk-rust-community/compare/v0.3.1...HEAD
[0.3.1]: https://github.com/community-ports/typesafeai-sdk-rust-community/releases/tag/v0.3.1
[0.3.0]: https://github.com/community-ports/typesafeai-sdk-rust-community/releases/tag/v0.3.0
[0.2.0]: https://github.com/community-ports/typesafeai-sdk-rust-community/releases/tag/v0.2.0
[0.1.0]: https://github.com/community-ports/typesafeai-sdk-rust-community/releases/tag/v0.1.0
