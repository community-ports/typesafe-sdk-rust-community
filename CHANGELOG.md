# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-09-18

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
- `tracing` output under the `typesafe_sdk` target with secret-header redaction.
- `X-TypeSafe-SDK`, `X-TypeSafe-Runtime` (runtime-detected OS and architecture), and
  `User-Agent` identification headers.

[Unreleased]: https://github.com/JSBtechnologies/typesafe-sdk-rust-community/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/JSBtechnologies/typesafe-sdk-rust-community/releases/tag/v0.1.0
