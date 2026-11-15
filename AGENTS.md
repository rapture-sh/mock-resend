# AGENTS.md

Guide for humans and AI agents working in this repo. Read this before writing code.

## What mock-resend is

mock-resend is a Resend-compatible email API for local development and testing. It
speaks enough of the Resend HTTP API that the official SDKs cannot tell the difference,
captures every message instead of delivering it, walks each one through the real
delivery event lifecycle, and fires signed webhooks at the caller's handler. Test suites
point at it instead of a real Resend account.

It is **not a deployment target.** It runs on a developer machine or in CI, and nowhere
else. Published images go to a registry so those environments can pull them, not so it
can be hosted.

It is part of [Rapture.sh](https://rapture.sh) and is MIT licensed.

**Status: pre-implementation.** The crates described below are being built out. This
file is the contract new code has to meet, not a description of code that already
exists. Where it names a reference implementation that has not landed yet, follow the
steps literally instead.

## The rules that matter

1. **Match real Resend, or document the divergence.** Same status code, same error
   `name`, same JSON shape, same field names, same headers. A mock that is subtly wrong
   is worse than no mock, because it makes broken code pass. When matching Resend is
   genuinely impossible — a real MTA, a real inbox, a real domain's DNS — list it as out
   of scope in the README rather than inventing plausible-looking behaviour.
2. **It never delivers mail.** There is no MTA here and there will not be one. No code
   in this repo opens a connection to a recipient's mail server, resolves an MX record,
   or hands a message to a local sendmail. A captured message is stored, reported and
   forgotten. This is the one rule that is not a trade-off against anything, and the
   most serious bug class in the project is a path that breaks it. See `SECURITY.md`.
3. **`mock-resend-core` knows nothing about HTTP or JSON.** No `axum`, no `hyper`, no
   `serde_json` in its dependencies. It owns the mailbox, the event lifecycle,
   scheduling, idempotency and fault rules, and reaches the filesystem through the
   `Store` trait. `tokio` for async I/O is fine; a web framework or a JSON serialiser is
   not. If testing a mailbox change requires binding a port, the logic is in the wrong
   crate.
4. **Everything is scoped to the API key.** Captured mail, delivery events, webhook
   endpoints, fault rules, statistics. No global switches, ever. A single instance has
   to be safe for a parallel test suite: a test that makes every send return `429` must
   not affect a test working under a different key. Any `re_`-prefixed string is a valid
   key and creates its namespace on first use, which is what makes the isolation free.
5. **A caller-supplied string is never a path, and a caller-supplied URL is never
   fetched unguarded.** Attachment filenames, message ids and header values are
   arbitrary strings, and clients send `../`, leading slashes and drive prefixes. They
   are resolved into the data directory by the one helper in `core::store` that does it
   safely, never by joining onto a `PathBuf`. The other half of this rule is outbound:
   an attachment `path` and a webhook endpoint are both URLs the caller chose, so
   fetching one is a server-side request forgery on the developer's machine. Outbound
   requests happen in one place, through the one guarded client in
   `mock-resend-webhooks`.

   That guard is **inverted relative to a normal SSRF policy**, and deliberately so. A
   normal guard blocks private addresses and allows the internet. Here every legitimate
   webhook endpoint is a handler on `localhost` or a neighbouring container, and the
   dangerous case is a typo posting captured mail out to a stranger. So loopback and the
   private ranges are allowed, the public internet is refused unless
   `--allow-public-webhooks` is set, and link-local is refused either way because
   `169.254.169.254` is where a cloud metadata service lives and CI is where this runs.
   Redirects are not followed: a `302` is a second URL the caller did not choose and the
   guard never saw.
6. **Start-up time and binary size are features.** They are most of why this exists.
   Prefer the standard library and the settled dependencies over adding another one.
7. **Never block the async runtime.** No synchronous file I/O and no long CPU work in a
   request handler. Disk is the default backend and MIME parsing runs on a socket, so
   this is on the common path, not an edge case: use async I/O or `spawn_blocking`.
8. **This is a test tool, not a mail server.** Any `re_`-prefixed key is accepted and
   nothing is authenticated. Do not add authentication, TLS or encryption, and do not
   treat their absence as a bug. Captured mail is not durable and carries no consistency
   guarantee. See `SECURITY.md`.

## Stack

- Rust, edition 2024, version pinned in `rust-toolchain.toml`
- `tokio` + `axum` + `hyper` for the HTTP surface
- `serde` + `serde_json` for the Resend wire format
- `bytes` for message bodies and attachment payloads
- `uuid` for email ids, `chrono` for the timestamps Resend reports
- `clap` for the CLI, `tracing` for logs
- `thiserror` for typed errors
- `resend-rs` as a dev-dependency, to drive the conformance suite

Landing with the phases that need them, not before: a MIME builder and parser for the
`.eml` on disk and the SMTP listener, `hmac`/`sha2`/`base64` for Svix-compatible webhook
signatures, an HTTP client for webhook delivery, and `rust-embed` for the console assets.
Add each one in the PR that first uses it, with the justification `CONTRIBUTING.md` asks
for.

## Commands

```bash
cargo run                    # API on :9100, SMTP on :1025
cargo test --workspace       # all tests
cargo fmt --all              # format
cargo fmt --all --check      # format check
cargo clippy --all-targets --all-features -- -D warnings
cargo xtask dist             # release binary
cargo xtask docker           # container image
```

Before you call work done:
`cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --workspace`.

## Layout

```
crates/
  mock-resend-core/          the engine. no HTTP, no JSON.
    src/model.rs             EmailId, ApiKey, Address, Attachment, Tag, Message
    src/mailbox.rs           capture, retrieve, list, reset
    src/events.rs            the delivery event lifecycle and its transitions
    src/schedule.rs          scheduled_at, cancellation, the advanceable clock
    src/idempotency.rs       key to result, the 24 hour window
    src/fault.rs             per-key fault rules
    src/store/mod.rs         the Store trait, and safe filename resolution
    src/store/disk.rs        default backend, writes under --data
    src/store/memory.rs      --in-memory backend

  mock-resend-api/           the Resend REST surface
    src/router.rs            route table
    src/auth.rs              Bearer parsing, and the key-to-namespace mapping
    src/error.rs             engine errors to Resend error names and statuses
    src/wire/                request and response types
    src/routes/emails.rs     send, batch, retrieve, update, cancel

  mock-resend-smtp/          the :1025 listener, MIME in, same mailbox
  mock-resend-webhooks/      outbound delivery, Svix-compatible signing, retries
  mock-resend-admin/         the /_admin API, and later the embedded console

  mock-resend/               the binary
    src/cli.rs               clap definitions
    src/main.rs              listener wiring

  mock-resend-testkit/       spawn an in-process instance from a Rust test
  conformance/               integration tests driven by a real Resend SDK

xtask/                       build orchestration
```

Where things go:

- `mock-resend-core` is the engine: input in, result out, with storage behind the
  `Store` trait. No sockets, no JSON, no `axum` types. Most unit test coverage lives
  here, next to the code, because this is where the genuinely tricky parts are — the
  event lifecycle's legal transitions, the idempotency window, scheduling against a
  clock that tests can move.
- `mock-resend-api` is the only crate that knows Resend is JSON over HTTP. Handlers
  parse, call the engine, and map errors. No business logic in a handler.
- `mock-resend-smtp` is a second way into the same engine, not a second mailbox. A
  message that arrives over SMTP is retrievable through `GET /emails/{id}` and produces
  the same events as one sent through the API.
- `mock-resend-webhooks` is the only crate that makes an outbound request, which is why
  it is a crate boundary rather than a module: rule 5 is enforceable when there is
  exactly one place to audit.
- `mock-resend-admin` is the `/_admin` surface — reset, inspect, advance an event, move
  the clock, inject a fault — and later the embedded console assets. It is mounted on
  the API listener under `/_admin`, which is safe because Resend's own paths are a fixed
  set that will never include it.
- `mock-resend` is wiring only: parse arguments, build the engine, start the listeners.
  Keep `main.rs` thin.
- `conformance` holds tests only and is never published. It depends on a real Resend
  SDK, which is the point.
- `xtask` is a plain Rust binary standing in for a Makefile, so no extra tool is needed
  on any platform.

## Adding an endpoint

1. **`mock-resend-core`** — the behaviour, as methods on the engine, with unit tests
   beside them in a `#[cfg(test)]` module. Invalid input returns a typed error variant,
   never a panic and never a string.
2. **`mock-resend-api/src/wire/`** — request and response types. Match the real field
   names and nesting exactly. Mind the fields Resend accepts as either a string or an
   array of strings (`to`, `cc`, `bcc`, `reply_to`); getting that wrong is the single
   most common way a mock diverges without anyone noticing.
3. **`mock-resend-api/src/routes/`** — the handler: parse the request, call the engine,
   map the error. Thin.
4. **`mock-resend-api/src/router.rs`** — wire up the route.
5. **`mock-resend-api/src/error.rs`** — add any new error `name` with its status. The
   [Resend errors reference](https://resend.com/docs/api-reference/errors) is the source
   of truth, and it is not a one-to-one mapping: `validation_error` appears under `400`,
   `401` and `403` depending on what failed. Guessing produces a mock that error
   handling cannot be tested against.
6. **`crates/conformance/tests/`** — a test driving the endpoint through a real Resend
   SDK. **Required for anything that adds API surface.** Hand-written HTTP proves the
   handler matches your understanding; the real SDK proves it matches what users
   actually send.
7. Update the supported-surface table in `README.md`.

An endpoint is not done until the conformance test passes and the README lists it.

## Shared parts

Reuse these rather than building another one. They are what keeps every endpoint
behaving like the same server. They land with the first endpoints; if one is missing
when you need it, build it there rather than inline in a handler.

- `core::mailbox` — capture, retrieve, list and reset, all keyed by API key. Every path
  that stores or reads a message goes through it.
- `core::events` — the lifecycle and which transitions are legal. An event is never
  appended from a handler; the engine decides whether `delivered` can follow `bounced`.
- `core::schedule` — `scheduled_at`, cancellation, and the clock. The clock is a
  dependency, never `Utc::now()` called inline, because advancing it is a feature and
  tests depend on it.
- `core::store::Store` — the storage trait. Disk is the default and `--in-memory` is the
  opt-out, so every operation has to work on both. New storage behaviour is a method
  here, not a branch on the backend type. Filename resolution lives here too, and is the
  only place allowed to turn a caller-supplied string into a filesystem path.
- `core::fault` — fault rules are consulted once per request, early, before any work. An
  endpoint that does not check them cannot be tested for error handling.
- `api::error` — the one place engine errors become Resend error names. A handler never
  writes an error body itself.
- `api::wire` — the serialisation types. Never hand-format JSON in a handler, and never
  reach for `serde_json::json!` to build a response that has a type.
- `webhooks` — the one guarded outbound client. Nothing else in the workspace makes an
  outbound request.
- `testkit` — spawns an in-process instance on a random port. Rust tests use it instead
  of shelling out to a container.

One rule comes with them: **message bodies and attachment payloads are `bytes::Bytes`,
never `Vec<u8>`.** Cloning `Bytes` bumps a refcount, so handing one message to the
mailbox, the event stream and a webhook copies nothing. A `to_vec()` in a hot path is a
bug.

## Code style

- `rustfmt` decides formatting; run `cargo fmt --all` rather than arguing with it.
- **Clippy warnings are errors.** If a lint is genuinely wrong, `#[allow]` it with a
  comment explaining why, in the narrowest scope that works.
- **No `unsafe`.** If you think you need it, open an issue first.
- No `unwrap()` or `expect()` outside tests, build scripts and `main`. Model failure as
  a typed error with `thiserror` and handle it at the boundary. This goes double in
  `mock-resend-smtp`, which parses hostile input off a socket.
- Public items that cross a crate boundary are documented; everything else is private by
  default. Do not make something `pub` to test it — test it from inside the module.
- Name things after the Resend concept they implement, in Rust casing: `POST /emails`
  becomes `send_email`, `email.delivery_delayed` becomes `DeliveryDelayed`. Someone
  reading the Resend docs should be able to find the code by name.
- Wire types use Resend's field names verbatim via `serde` attributes. Do not rename a
  field to read better in Rust and translate it in the handler.
- Import groups in order: `std`, external crates, then `crate`. `rustfmt` keeps them
  tidy; do not hand-roll a different arrangement.
- Error messages are for humans: say what is wrong with the request, in plain words.
- Log with `tracing`, never `println!`. One span per request, and **never log a message
  body, an attachment, a subject or a recipient address.** Captured mail is sometimes
  real data a developer pointed at this by mistake; an id and an event name are enough.

## Comments

- Code should read on its own. Do not write a comment that repeats what the next line
  already says, and do not use banner comments to label sections.
- Use rustdoc (`///`) only where the code cannot say it itself: a contract the types do
  not capture, an invariant the caller has to keep, a unit, the error a function
  returns, or the reason something is done an unusual way. Explain why, not what.
- **Cite Resend behaviour where it is surprising.** A one-line comment saying that
  Resend returns `200` here rather than `201`, that a webhook payload reports `tags` as
  an object while the send request takes an array of `{name, value}`, or that
  `last_event` spells it `canceled` with one `l`, with a doc link, is worth more than any
  other comment in this repo. The next person will otherwise assume it is a bug and
  "fix" it.
- Nothing commented out, no `TODO` without a linked issue, no changelog inside a file.
- A stale comment is worse than no comment. If the code moves on, delete the comment.

## Testing

Two tiers, and they do different jobs.

**Unit tests** live beside the code in a `#[cfg(test)]` module, and belong to
`mock-resend-core`. They cover the logic: event transitions, the idempotency window,
scheduling and cancellation against a controlled clock, validation, fault matching,
filename resolution. They bind no ports and run in milliseconds. This is where coverage
counts.

**Conformance tests** live in `crates/conformance/tests/` and drive a real instance
through a real Resend SDK. They cover the wire: the auth requirement, field names, the
string-or-array fields, status codes, error names, and webhook signatures a real verifier
accepts. Anything that adds API surface needs one.

Conventions:

- **Every test name starts with `should`**, reads as a sentence, and says the case it
  covers. Add `when` where the case needs saying.

  ```rust
  mod send_email {
      #[test]
      fn should_accept_a_single_recipient_as_a_bare_string() {}

      #[test]
      fn should_reject_more_than_fifty_recipients() {}
  }
  ```

  A `mod` names the thing under test, so an endpoint or a type, and carries no
  `should`.

- Use `testkit` to get an instance, never a hardcoded port. Tests run in parallel and a
  fixed port makes them flaky.
- Tests use the in-memory backend unless the test is about disk behaviour. Storage tests
  that need disk get their own temporary directory and clean it up.
- **Give each test its own API key, named after the test.** That is the isolation
  mechanism; rule 4 exists to make it work.
- Never let a test depend on wall-clock time passing. Scheduling and delivery delays are
  tested by moving the clock, not by sleeping.
- Assert the error **`name`**, not the message text. `validation_error` is the contract;
  the wording is not.

Tests have to earn their place. A test is worth keeping only if it can fail for a real
reason:

- Test behaviour, not structure: what a caller actually gets back. Cover the happy path,
  empty input, invalid input, boundaries, round trips, and any bug that was just fixed.
- No test that restates the implementation or asserts a mock was called with the
  argument you just handed it.
- Do not test `tokio`, `axum`, `serde` or a Resend SDK. Assume they work.
- Avoid arbitrary thresholds. Assert the invariant you actually care about.
- A few tests that would catch a real break beat many that pad the count.
- Test files follow everything else in this file, the comment rules included. That
  includes the fixtures: use `example.com` and `example.org` addresses, never a real
  one, and never a real API key even in a comment.

## Benchmarks

Performance claims need numbers. If a change is meant to be faster, say what you
measured, how, and the before and after. "Should be faster" is not a benchmark, and a
microbenchmark of something that happens once per process start is not a result.

## Agent playbooks

`.agents/` holds one playbook per role in plain Markdown, with no vendor frontmatter, so
any coding agent can be pointed at them:

- `.agents/endpoint-builder.md` implements a Resend endpoint end to end.
- `.agents/fidelity-auditor.md` audits a diff for divergence from real Resend and for
  anything that breaks the crate boundaries.
- `.agents/test-author.md` writes the tests that are missing.

For Claude Code, `.claude/agents/` wraps each playbook as a subagent and
`.claude/commands/` adds `/new-endpoint` and `/check`. The instructions themselves stay
in `.agents/` only, so nothing has to be kept in sync.

Rules that apply to everyone belong in this file, not in a playbook.

## Git

- Branch from `main`: `feat/<short-name>` or `fix/<short-name>`.
- **Commit messages are `feat: ...` or `fix: ...` only, and at most two sentences.**
  No other prefixes, no bodies that ramble, no emoji.

  ```
  feat: add POST /emails/batch with the 100-message limit
  fix: return validation_error instead of 500 for a tag containing a space
  ```

- **No co-author trailers, no tool attribution.** A commit message is the one
  `feat:`/`fix:` line and nothing else. Never append `Co-Authored-By:` trailers,
  `Generated with ...` footers, or any other credit for an AI agent, editor or tool.
  Not in commit messages, not in PR descriptions. The human who opened the PR is the
  author.
- One logical change per PR. Keep the diff readable.
- `Cargo.lock` is committed. This is a binary, not a library.
