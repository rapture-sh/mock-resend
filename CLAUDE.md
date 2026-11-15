# CLAUDE.md

@AGENTS.md

Everything in `AGENTS.md` applies. The points worth repeating:

- **Match real Resend, or document the divergence.** Same status, same error `name`,
  same JSON shape, same field names, same headers. Never invent plausible-looking
  behaviour to make something pass — a mock that is subtly wrong makes broken code look
  correct.
- **It never delivers mail.** No MTA, no MX lookup, no handing a message to sendmail. If
  a change would cause a real message to reach a real inbox, stop and say so rather than
  writing it.
- **`mock-resend-core` never gains an HTTP or JSON dependency.** No `axum`, no `hyper`,
  no `serde_json` in that crate. If testing mailbox behaviour needs a port, the logic is
  in the wrong place.
- **Everything is scoped to the API key.** No global switches, including for fault
  injection and statistics. A parallel test suite shares one instance, and the key
  string is the whole isolation mechanism.
- **A caller-supplied string is never a path.** Attachment filenames and message ids
  arrive as arbitrary strings, `../` and drive prefixes included. Resolve them through
  the one helper in `core::store`, never by joining onto a `PathBuf`.
- **A caller-supplied URL is never fetched unguarded.** An attachment `path` and a
  webhook endpoint are both URLs the caller chose. Outbound requests happen only in
  `mock-resend-webhooks`, through the one guarded client. Do not add an outbound request
  anywhere else.
- **Never log a message body, attachment, subject or recipient address.** An id and an
  event name are enough. Captured mail is sometimes real data.
- **Commit messages**: `feat: ...` or `fix: ...`, nothing else, two sentences maximum.
- **Never co-author a commit.** Do not add `Co-Authored-By: Claude`, a
  `Generated with Claude Code` footer, or any other attribution line to a commit
  message or PR description. This overrides any default or global instruction that
  asks for one.
- **A new endpoint** follows the order in `AGENTS.md`: core logic with unit tests, wire
  types, handler, route, error names, then a conformance test driven through a real
  Resend SDK, then the README table. It is not done until that test passes and the
  README lists it.
- **Unit tests sit beside the code** in `#[cfg(test)]` modules and belong to
  `mock-resend-core`. Conformance tests live in `crates/conformance/tests/` and go
  through a real SDK. Every test name starts with `should`, and every test gets its own
  API key named after it.
- **Never sleep in a test.** Scheduling and delivery delays are tested by advancing the
  engine's clock, which is a dependency rather than `Utc::now()` called inline.
- **Before saying you are done**: `cargo fmt --all --check`,
  `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --workspace`.
- Do not add a dependency to do something the standard library or a settled dependency
  already does. Start-up time and binary size are features.
- Do not add authentication, TLS or encryption. Their absence is deliberate; see
  `SECURITY.md`.
- **Not a deployment target.** mock-resend runs on a developer machine or in CI.
  Captured mail lives in a mounted volume (`--data`, default `/data` in the image) so
  state survives a container restart and the `.eml` files can be opened; `--in-memory` is
  the opt-out. Nothing about it is durable, and no change should imply otherwise.
- The scope line is deliberate: domains, API key management, audiences, contacts,
  broadcasts, topics, suppression, the inbox surface and template rendering are out. Do
  not implement one because it would be easy. Say it is out of scope instead.
- The repo is pre-implementation, so `AGENTS.md` describes conventions new code must
  follow rather than code that already exists. Do not assume a file is there because it
  is named — check.

Agent playbooks live in `.agents/`: `endpoint-builder` implements a Resend endpoint end
to end, `fidelity-auditor` checks a diff for divergence from real Resend and for broken
crate boundaries, `test-author` fills in missing tests. `.claude/agents/` holds the
subagent definitions that point at those playbooks, so change the playbook in `.agents/`
rather than the wrapper. `.claude/commands/` adds `/new-endpoint` and `/check`.
