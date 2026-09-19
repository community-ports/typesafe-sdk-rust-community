# Contributing

Issues and pull requests are welcome.

## Ground rules

- This crate tracks the official [Python SDK](https://github.com/typesafe-ai/typesafe-sdk-python).
  When behavior is ambiguous, match the Python SDK and the
  [TypeSafe docs](https://docs.typesafe.ai/), and note the source in the PR.
- Public API changes need a `CHANGELOG.md` entry under `[Unreleased]`.
- Keep the crate offline-testable: exercise HTTP behavior against the scripted mock server in
  `tests/support`, and put anything that needs a real API key in `tests/live.rs`, which skips
  itself when `TYPESAFE_API_KEY` is unset.

## Checks

CI runs the following on Linux, macOS, and Windows; please run them locally first:

```sh
cargo fmt --all -- --check
cargo clippy --all-features --all-targets -- -D warnings
cargo clippy --no-default-features --features native-tls -- -D warnings
cargo test --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps
```

## Releasing

1. Bump `version` in `Cargo.toml` and move the `[Unreleased]` notes into a new section.
2. Commit, tag `vX.Y.Z`, and push the tag.
3. `cargo publish` (once the crate is on crates.io).
