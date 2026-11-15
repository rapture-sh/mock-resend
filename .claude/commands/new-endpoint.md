---
description: Implement a Resend endpoint end to end
argument-hint: <METHOD /path> [notes]
---

Implement this Resend endpoint in mock-resend: $ARGUMENTS

Use the `endpoint-builder` agent. It must follow `AGENTS.md` and build in that order:
engine logic in `mock-resend-core` with `#[cfg(test)]` unit tests, wire types, the
handler, the route, any new error names, then a conformance test in
`crates/conformance/tests/` driven through a real Resend SDK, then the supported-surface
table in `README.md`.

Confirm the real behaviour against the Resend API reference before writing the handler —
status codes, field names and error names are not to be guessed, and the docs do not
publish the error body shape, so check it against an SDK. If the reference is ambiguous,
stop and tell me rather than picking something plausible.

Check the scope line first: if this endpoint is one of the deliberately excluded ones
(domains, API keys, audiences, contacts, broadcasts, topics, suppression, inbox,
template rendering), say so and stop instead of building it.

When it is built, run `cargo fmt --all`,
`cargo clippy --all-targets --all-features -- -D warnings` and `cargo test --workspace`,
then report the files you touched and the Resend behaviour you matched.
