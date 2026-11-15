# Contributing to mock-resend

Thanks for being here. mock-resend is pre-release, which means there is a lot of
obvious work available and you do not need permission to start. Open an issue if
you want to talk something through first.

## The contract

Read [AGENTS.md](AGENTS.md). It is the actual contract for this repo — layout, code
style, testing conventions, and the rules below in full. Everything here assumes it.

Using an AI agent to help? Point it at `AGENTS.md` and the matching playbook in
`.agents/`. The same rules apply to code it writes, including the one about attribution
in commit messages.

One rule outranks everything else in this repo:

> **Match real Resend, or document the divergence.** If an SDK sends a request that
> real Resend answers one way, mock-resend answers the same way: same status, same
> error `name`, same JSON shape, same headers. A mock that is subtly wrong is worse
> than no mock, because it makes broken code pass.

When you genuinely cannot match Resend — because the honest implementation needs a
real MTA, or a real inbox — say so in the README under "out of scope" rather than
inventing plausible-looking behaviour.

The second rule is structural:

> **`mock-resend-core` knows nothing about HTTP or JSON.** No `axum`, no `hyper`, no
> `serde_json` in its dependency list. The mailbox, the event lifecycle, scheduling,
> idempotency and fault rules reach storage through the `Store` trait. If testing a
> mailbox change requires binding a port, the logic is in the wrong crate.

The third is what makes the whole thing usable in a test suite:

> **Everything is scoped to the API key.** Captured mail, events, webhook endpoints,
> fault rules, statistics. No global switches, ever. One instance has to be safe for a
> parallel suite, and the key string is the only isolation mechanism there is.

## Good contributions

- **A missing endpoint on the mailing path.** Check the supported-surface table in
  the README. Anything absent and in scope is fair game.
- **A fidelity bug.** Somewhere mock-resend returns the wrong status, error `name`,
  field name or header compared to real Resend. These are the most valuable reports,
  and even a report without a fix is useful.
- **Event lifecycle and webhook work.** More faithful signing, retry behaviour,
  event payload fields we have wrong.
- **SMTP.** MIME edge cases, encodings, attachments arriving over the wire.
- **Performance.** With a benchmark showing the before and after. Not a guess.
- **Documentation.** If something took you an hour to figure out, that is a bug
  in the README.

Not sure if an idea fits? Open an issue describing it. That is cheaper than
building it and finding out.

Be aware of the scope line before you start on an endpoint: domains, API key
management, audiences, contacts, broadcasts, topics, suppression and the inbox
surface are deliberately out. If you want one of them in, make the case in an issue
first — say what you are trying to test — rather than opening a PR for it.

## Setup

Rust is pinned in `rust-toolchain.toml`; rustup picks it up automatically.

```bash
cargo run
```

That starts the API on `:9100` and SMTP on `:1025`, writing captured mail to `./data`.
Pass `--in-memory` if you would rather it left nothing behind.

## Adding an endpoint

Follow the shape of an existing one — `POST /emails` is the reference.

1. **`crates/mock-resend-core/`** — the behaviour, as methods on the engine. Unit
   tests next to the logic for the interesting state transitions and edge cases.
2. **`crates/mock-resend-api/src/wire/`** — request and response types. Match the
   real field names, nesting and `snake_case` exactly, including which fields accept
   both a string and an array.
3. **`crates/mock-resend-api/src/routes/`** — the handler. Parse, call the engine, map
   errors through `error.rs`. No business logic here.
4. **`crates/mock-resend-api/src/router.rs`** — wire up the route.
5. **`crates/conformance/tests/`** — a test driving it through a real Resend SDK.
   **This is required for anything that adds API surface.** Hand-written HTTP requests
   prove your handler matches your understanding; the real SDK proves it matches what
   users will send.
6. Update the supported-surface table in the README.

For error cases, the
[Resend errors reference](https://resend.com/docs/api-reference/errors) is the source
of truth for `name` and status pairings. Guessing produces a mock that error handling
cannot be tested against.

## Dependencies

Default answer is no, and it matters more here than in most projects: start-up
time and binary size are features. A dependency needs to earn its place in the
PR description — what it does, why the standard library or an existing dependency
cannot, and what it costs in compile time.

`tokio`, `axum`, `serde`, `serde_json`, `clap` and `tracing` are settled. Adding
another async runtime or HTTP stack is not a PR, it is a discussion.

## Checks

Run these before opening a PR, and make sure they pass:

```bash
cargo fmt --all --check
```

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

```bash
cargo test --workspace
```

Clippy warnings are errors here. If a lint is genuinely wrong, `#[allow]` it with
a comment explaining why, in the narrowest scope that works.

## Commits

Commit messages are `feat: ...` or `fix: ...` and **at most two sentences**. No
other prefixes, no essays, no emoji.

```
feat: add POST /emails/batch with the 100-message limit
fix: return validation_error instead of 500 for a tag with a space in it
```

No `Co-Authored-By:` trailers and no tool or AI attribution footers, in commit
messages or PR descriptions. The person opening the PR is the author.

Branches: `feat/<short-name>` or `fix/<short-name>`, cut from `main`.

## Pull requests

- One logical change per PR.
- Say what it does and why. For a fidelity fix, say what real Resend does and how
  you know — a doc link or an observed response.
- Conformance test for new API surface; regression test for any bug fixed.
- Note any new dependency and why it earned its place.
- Benchmark numbers for anything claiming to be faster.

Reviews are about the code, never the person. See
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

## Forking

Fork it, run your own copy, build something else on top; that is what MIT is for.
Keep the copyright notice and credit where the code came from, and we are square.
