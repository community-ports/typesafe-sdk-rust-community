# Security Policy

## Supported versions

Only the latest release on crates.io receives security fixes.

## Reporting a vulnerability

Please do not open a public issue for security problems.

Use GitHub's private vulnerability reporting for this repository
("Security" tab → "Report a vulnerability"). While the repository is private
and that option is unavailable, contact the maintainer through the email on
their GitHub profile instead. You can expect an acknowledgement within a few
days and a fix or mitigation plan before any public disclosure.

If the problem is in the TypeSafe AI API itself rather than this SDK, report it
to TypeSafe AI directly at <support@typesafe.ai>.

## Scope notes

- This crate never logs credentials: `Authorization` and other secret-bearing
  headers are redacted from `tracing` output. Request and response bodies are
  logged at `DEBUG` level unredacted, so avoid enabling `DEBUG` in production
  if your `state` contains sensitive data.
- The API key is sent only to the configured `base_url` over HTTPS. Pointing
  `TYPESAFE_BASE_URL` at another host sends your key there.
- Dependencies are monitored by Dependabot, Socket, and `cargo-deny`
  (RustSec advisories and a license allow-list) in CI.
