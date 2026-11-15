# Endpoint builder

**When to use:** Use when implementing a Resend endpoint in mock-resend, or reworking an existing one. Builds the core engine logic, the wire types, the handler, the route, the error names and the conformance test, following the conventions in AGENTS.md.

You implement Resend endpoints in mock-resend. Read `AGENTS.md` first. If an endpoint has
already landed, read it and copy its shape; the repo is pre-implementation, so if none
has, follow the order below literally and you are setting the pattern.

Hard constraints:

- **Match real Resend.** Same status code, same error `name`, same JSON field names and
  nesting, same headers. Check the
  [Resend API reference](https://resend.com/docs/api-reference/introduction) for the
  endpoint and the
  [errors reference](https://resend.com/docs/api-reference/errors) for the failure cases.
  Do not guess a status code or an error name. If the real behaviour is unclear, say so
  rather than picking something plausible — the docs do not publish the error body shape,
  for instance, so confirm it against an SDK rather than inventing one.
- **It never delivers mail.** Nothing you write opens a connection to a recipient's mail
  server or resolves an MX record. A captured message is stored, reported and forgotten.
- **`mock-resend-core` gains no HTTP or JSON dependency.** Engine logic takes parsed
  input and returns a typed result. If you find yourself wanting an `axum` type or
  `serde_json::Value` in `core`, the split is wrong.
- **Scope state to the API key.** No global maps, no process-wide flags, including for
  anything you add to fault injection or statistics.
- **Message bodies and attachment payloads are `bytes::Bytes`.** No `to_vec()` in a
  request path.
- **A caller-supplied string is never a filesystem path.** Attachment filenames and
  message ids go through the one resolver in `core::store`; never join one onto a
  `PathBuf`.
- **A caller-supplied URL is never fetched outside `mock-resend-webhooks`.** An
  attachment `path` is a URL the caller chose. If the endpoint needs one fetched, route
  it through the guarded client rather than reaching for an HTTP client yourself.
- **Never log a body, attachment, subject or recipient address.**
- **No blocking I/O in a handler.** Disk work is async or `spawn_blocking`.
- If the endpoint needs a new dependency, stop and say why before adding it.

Build it in this order:

1. **`crates/mock-resend-core/`** — the behaviour, as methods on the engine. Typed error
   variants with `thiserror`, never a panic, never a stringly-typed error. Unit tests in
   a `#[cfg(test)]` module beside the code: happy path, empty input, invalid input,
   boundaries, and a round trip where one exists. Anything time-dependent takes the
   engine's clock rather than calling `Utc::now()`.
2. **`crates/mock-resend-api/src/wire/`** — the request and response types. Match the
   real field names and nesting exactly. Mind the fields Resend accepts as either a
   string or an array of strings (`to`, `cc`, `bcc`, `reply_to`), the validation limits
   (50 recipients, the tag character set, `content` or `path` on an attachment,
   `template` excluding `html` and `text`), and the fields that are absent rather than
   null. This is the most common place a mock diverges silently.
3. **`crates/mock-resend-api/src/routes/`** — the handler. Parse, call the engine, map
   the error. No business logic; if the handler is making decisions, they belong in
   `core`.
4. **`crates/mock-resend-api/src/router.rs`** — the route. Check that adding it has not
   shadowed an existing one, and that `/_admin` is still reachable.
5. **`crates/mock-resend-api/src/error.rs`** — any new error `name` with its HTTP status.
   Remember that `validation_error` is not tied to one status.
6. **`crates/conformance/tests/`** — a test driving the endpoint through a real Resend
   SDK, using `testkit` for the instance and an API key named after the test. Cover the
   success path and at least one error path.
7. **`README.md`** — add the endpoint to the supported-surface table.

Then run `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`
and `cargo test --workspace`, and fix what they report.

Report back with the files you created or changed, the engine API you added, the Resend
behaviour you matched and where you confirmed it, and anything you deliberately left
out.
