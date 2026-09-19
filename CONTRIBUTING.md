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
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo clippy --no-default-features --features native-tls -- -D warnings
cargo test --workspace --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps
cargo deny check   # cargo install cargo-deny
```

`Cargo.lock` is committed so CI, Dependabot, and Socket see the exact dependency tree;
GitHub Actions are pinned to commit SHAs and Dependabot keeps the pins current.

## Releasing

Releases go through crates.io Trusted Publishing; no registry token is stored anywhere.

1. Bump `version` in `Cargo.toml` and `macros/Cargo.toml` (the main crate pins the macros
   crate with `=X.Y.Z`, so they move together), update the install snippets in `README.md` to
   the new minor version, and move the `[Unreleased]` notes into a `## [X.Y.Z] - date` section
   in `CHANGELOG.md`. Commit and push to `main`. The release workflow refuses a tag whose
   README, changelog, or crate versions disagree.
2. Create a signed tag and push it: `git tag -s vX.Y.Z -m "vX.Y.Z" && git push origin vX.Y.Z`.
3. The `Release` workflow checks the tag against both `Cargo.toml` files and the changelog,
   waits for a maintainer to approve the `release` environment, runs the tests, publishes the
   macros crate and then the main crate with an OIDC token (skipping any version already on
   crates.io), and creates the GitHub release from the changelog section.
