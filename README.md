mock-resend
===========

A Resend-compatible email API for local development and testing. It speaks enough of the
Resend HTTP API that the official SDKs cannot tell the difference, captures every message
instead of delivering it, walks each one through the real delivery event lifecycle, and
fires signed webhooks at your handler so the half of your code that reacts to email can be
tested too.

Written in Rust, with no JVM and no runtime dependencies. Ships as a single static binary
in a container that starts in milliseconds.

> **This is not a deployment target.** mock-resend is for local development environments
> and CI. It accepts any API key, exposes an unauthenticated admin API, and **never
> delivers mail to a real recipient** — that is the entire point, and it is also why
> nothing about it is safe to expose. Bind it to localhost or a container network and keep
> it there. See [SECURITY.md](SECURITY.md).

It started inside the [Kywi](https://github.com/rapture-sh) monorepo, standing in for
Resend across that test suite. Other projects needed the same thing, so it was extracted
here, rebuilt around a standalone engine and extended well past the original — the full
delivery event lifecycle with real signed webhooks, an SMTP listener sharing one mailbox
with the API, scheduled sends you can fast-forward, idempotency keys, per-key isolation
for parallel suites and fault injection. Kywi now consumes it like everyone else.

**Status: pre-release.** This describes the target shape of the first tagged release, not
a finished product. The admin API will break without ceremony until `0.1.0`. See the
[issues](https://github.com/rapture-sh/mock-resend/issues) for what is wired up today.


Running it
----------

```bash
docker run --rm -p 9100:9100 -p 1025:1025 -v mock-resend-data:/data ghcr.io/rapture-sh/mock-resend:latest
```

Resend API on `:9100`, SMTP on `:1025`. Captured mail lives in the volume mounted at
`/data`, so it survives a container restart and you can open the `.eml` files from the
host.

```yaml
services:
  email:
    image: ghcr.io/rapture-sh/mock-resend:latest
    ports:
      - "9100:9100"
      - "1025:1025"
    volumes:
      - mock-resend-data:/data

volumes:
  mock-resend-data:
```

Run without a volume and the container keeps captured mail in its own writable layer,
which disappears with the container. For a suite that wants nothing on disk at all, pass
`--in-memory`.


Pointing a client at it
-----------------------

Any API key works, as long as it looks like one — a `re_` prefix is all that is checked.
The key is not authentication here, it is the **namespace** (see Architecture), so give
each test its own.

The Node SDK takes a base URL in its constructor, and both the Node and Rust SDKs read
`RESEND_BASE_URL` from the environment:

```ts
const resend = new Resend("re_test", { baseUrl: "http://localhost:9100" });
```

For anything without a base-URL switch, point your HTTP client at `http://localhost:9100`
instead of `https://api.resend.com`; the paths and payloads are identical.

```bash
curl -X POST http://localhost:9100/emails -H "Authorization: Bearer re_test" -H "Content-Type: application/json" -d @email.json
```


Configuration
-------------

| Flag | Default | What it does |
| --- | --- | --- |
| `--data <dir>` | `/data` in the image, `./data` otherwise | Where captured mail is written |
| `--in-memory` | off | Keep everything in RAM and discard it on exit |
| `--port` | `9100` | Resend HTTP API port |
| `--smtp-port` | `1025` | SMTP listener port |
| `--no-smtp` | off | Disable the SMTP listener |
| `--rate-limit <n>` | off | Requests per second per key, with the `ratelimit-*` headers |
| `--log <level>` | `info` | `error`, `warn`, `info`, `debug`, `trace` |

Every flag has a `MOCK_RESEND_`-prefixed environment variable equivalent, which is usually
easier in a compose file.

Rate limiting is **off by default** even though real Resend limits you to 10 requests per
second. A test suite that trips a rate limit it did not ask for is a flaky test suite; a
suite that wants to prove its retry logic works turns the limit on deliberately.


Architecture
------------

One instance, one endpoint, many keys — the shape Resend itself has.

```
mock-resend process
├── :9100  Resend HTTP API  ──┐
├── :1025  SMTP             ──┼──>  engine  ──>  store  ──>  /data volume
└── admin API               ──┘       │
                                      └──>  webhook sender  ──>  your handler
```

Both listeners share one engine and one mailbox, so a message that arrives over SMTP is
retrievable through `GET /emails/{id}` and lands in the same event stream as one sent
through the API.

**Everything is scoped to the API key** — captured mail, delivery events, webhook
endpoints, fault rules, statistics. No global switches. That is what makes one shared
instance safe for a parallel test suite: a test that makes every send return `429` cannot
affect a test using a different key. Any `re_`-prefixed string is a valid key and creates
its namespace on first use, so isolation costs nothing:

```ts
const resend = new Resend("re_test_checkout_flow", { baseUrl: BASE_URL });
```

Two things make it pleasant in a suite: `POST /_admin/reset` wipes everything or a single
key, far faster than restarting a container; and per-key fault injection lets you make
sends fail with a given Resend error, or make a delivery bounce, so testing your error
handling stops needing a broken mail server.

Rust users can skip the container and use `mock-resend-testkit` to spawn an instance
in-process on a random port.


Supported surface
-----------------

| Area | Endpoints |
| --- | --- |
| Send | `POST /emails`, `POST /emails/batch` |
| Manage | `GET /emails/{id}`, `PATCH /emails/{id}`, `POST /emails/{id}/cancel` |
| Protocol | `Bearer` auth, the mandatory `User-Agent` check, `Idempotency-Key`, `ratelimit-*` headers |
| SMTP | Plain SMTP on `:1025`, username `resend`, password any API key |
| Admin | `POST /_admin/reset`, list captured mail, advance an event, inject a fault |

Validation matches Resend's, because validation is most of what a mock is for: the
50-recipient limit, the tag character set, `content` or `path` on an attachment but not
neither, `template` being mutually exclusive with `html` and `text`, and the
`Idempotency-Key` rules. A payload mock-resend accepts is a payload real Resend accepts.

Errors use Resend's own `name` and status pairings — `validation_error`,
`missing_required_field`, `invalid_attachment`, `rate_limit_exceeded`,
`invalid_idempotent_request` and the rest — so code that branches on an error name can be
tested against them.


Delivery events and webhooks
----------------------------

A captured message is not simply stored. It moves through the same lifecycle real Resend
reports, and every transition is emitted as a webhook to the endpoints registered for that
key.

| Event | When |
| --- | --- |
| `email.scheduled` | A send with `scheduled_at` in the future |
| `email.sent` | Accepted and handed off |
| `email.delivered` | Delivery succeeded, after a configurable delay |
| `email.delivery_delayed`, `email.bounced`, `email.complained`, `email.failed` | On demand, or from a fault rule |
| `email.opened`, `email.clicked` | On demand, so engagement handling is testable |

Webhooks are signed the way Resend signs them — `svix-id`, `svix-timestamp` and
`svix-signature` — so your real verification code runs unmodified instead of being stubbed
out in tests. That is what a mock that only stores mail cannot give you, and it is usually
where the bugs are.

Scheduled sends do not need you to wait: the engine's clock can be advanced through the
admin API, so a test for "this email goes out in 24 hours" takes a millisecond.


Out of scope
------------

Domains, API key management, audiences, contacts, broadcasts, topics, suppression lists and
the inbox, thread and draft surface. Template rendering is out too: a `template` id is
accepted, validated and recorded, but nothing is rendered from it.

These are not oversights. mock-resend exists so the code path that *sends* email can be
tested without the network, and every endpoint off that path is surface to keep faithful
for no return. If you need one of them, open an issue and say what you are trying to test —
that is the argument that moves something into scope.

Also out: real delivery. mock-resend has no MTA and never will. Nothing it accepts reaches
a real inbox.


Building from source
--------------------

Needs a [Rust](https://rustup.rs) toolchain, pinned in `rust-toolchain.toml`.

```bash
cargo run
```

```bash
cargo xtask dist
```

Layout, conventions, testing and style are in [AGENTS.md](AGENTS.md).


Contributions
-------------

Welcome, and you do not need permission to start. The pre-release state means there is
plenty of obvious work available.

Read [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md) first. The rule that
outranks the rest: match real Resend, or document the divergence. Commits are `feat:` or
`fix:` and at most two sentences, one logical change per PR, and anything adding API
surface comes with a conformance test driven through a real Resend SDK.

Conduct is covered by [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Security issues go through
[SECURITY.md](SECURITY.md), not a public issue.


Licensing
---------

[MIT](LICENSE).

Resend is a trademark of Resend, Inc. This project is not affiliated with or endorsed by
Resend; "Resend-compatible" describes the HTTP API it implements, nothing more.
