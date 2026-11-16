# mock-resend

A Resend-compatible email API for local development and testing. It speaks enough of the
Resend HTTP API that the official SDKs cannot tell the difference, captures every message
instead of delivering it, and walks each one through the real delivery event lifecycle,
firing Svix-signed webhooks at your handler so the half of your code that *reacts* to
email is testable too.

Written in Rust. No JVM, no runtime dependencies, around 24 MB. The same binary serves a
web console on the API port, so you can read what your code actually sent — preview, HTML,
plain text, raw `.eml` and attachments — without opening the volume. An SMTP listener on a
second port feeds the same mailbox, so a client that only speaks SMTP lands in the same
place.

> **Not a deployment target.** mock-resend is for development machines and CI. It accepts
> any API key, authenticates nothing, exposes an unauthenticated admin API, and makes no
> durability guarantee. **It never delivers mail to a real recipient** — that is the whole
> point, and also why nothing about it is safe to expose. Bind it to localhost or a
> container network and keep it there.

## Quick start

```bash
docker run --rm -p 9100:9100 -p 1025:1025 -v mock-resend-data:/data raptureoss/mock-resend
```

Resend API, admin API and console on `:9100`. SMTP on `:1025`. Captured mail lives in the
volume at `/data` as one `.eml` per message, so it survives a restart and you can open the
files in any mail client.

Drop the volume and captured mail goes to the container's writable layer instead, which
disappears with the container — which is usually what a CI job wants.

## Tags and architectures

`latest` tracks releases, never the main branch. In CI, pin a major such as `:1` so patches
reach you but a new major cannot land on you by surprise.

Built for `linux/amd64` and `linux/arm64`, so an Apple Silicon machine runs native code
rather than emulation.

## Pointing a client at it

**You do not need a different client.** Resend's own official SDKs take a custom base URL,
so the same `resend` package your production code imports is the one that talks to this —
no wrapper, no interface to stub, no second code path that only runs under test.

```ts
const resend = new Resend("re_test", { baseUrl: "http://localhost:9100" });
```

Both the Node and Rust SDKs also read it from the environment, which is usually the whole
integration:

```bash
RESEND_BASE_URL=http://localhost:9100
```

Any API key works, as long as it starts with `re_`. The key is not authentication here, it
is the **namespace**: every captured message, webhook endpoint and fault rule is scoped to
it, so a parallel suite stays isolated by giving each test its own key. A test that makes
every send return `429` cannot affect a test using a different one.

For anything without a base-URL switch, point your HTTP client at `http://localhost:9100`
instead of `https://api.resend.com`. The paths and payloads are identical.

```bash
curl -X POST http://localhost:9100/emails \
  -H "Authorization: Bearer re_test" \
  -H "Content-Type: application/json" \
  -d '{"from":"a@example.com","to":"b@example.org","subject":"hi","text":"hello"}'
```

[rapture-sh/mailer](https://github.com/rapture-sh/mailer) is the TypeScript client we pair
with this — Resend in production, mock-resend everywhere else, by swapping the base URL.
mock-resend does not depend on it and works with any Resend client.

## Compose

```yaml
services:
  email:
    image: raptureoss/mock-resend:1
    ports:
      - "9100:9100"
      - "1025:1025"
    volumes:
      - mock-resend-data:/data

volumes:
  mock-resend-data:
```

The image ships its own health check — the binary probes itself, so nothing else has to be
installed in it — which means `depends_on` with `condition: service_healthy` works without
you declaring anything:

```yaml
  app:
    depends_on:
      email:
        condition: service_healthy
```

## Configuration

Every setting is a flag and an environment variable. In a compose file the variable is
usually easier.

| Variable | Flag | Default | What it does |
| --- | --- | --- | --- |
| `MOCK_RESEND_DATA` | `--data` | `/data` | Where captured mail is written, one `.eml` per message |
| `MOCK_RESEND_PORT` | `--port` | `9100` | Resend API, admin API and console |
| `MOCK_RESEND_SMTP_PORT` | `--smtp-port` | `1025` | SMTP listener |
| `MOCK_RESEND_NO_SMTP` | `--no-smtp` | `false` | Do not start the SMTP listener |
| `MOCK_RESEND_HOST` | `--host` | `0.0.0.0` | Address both listeners bind to |
| `MOCK_RESEND_RATE_LIMIT` | `--rate-limit` | unset | Requests per second per key, with the `ratelimit-*` headers |
| `MOCK_RESEND_DELIVERY_DELAY` | `--delivery-delay` | unset | Milliseconds after a send before the message is delivered |
| `MOCK_RESEND_ALLOW_PUBLIC_WEBHOOKS` | `--allow-public-webhooks` | `false` | Let webhooks reach public internet addresses |
| `MOCK_RESEND_LOG` | `--log` | `info` | `error`, `warn`, `info`, `debug`, `trace`, or a full filter |

Boolean variables take `true` or `false` and nothing else. `MOCK_RESEND_NO_SMTP=1` is
refused at startup rather than quietly ignored.

Two defaults are deliberately unlike the real API. **Rate limiting is off**, though real
Resend allows 10 requests a second, because a suite that trips a limit it did not ask for
is a flaky suite — turn it on when you want to prove your backoff works. And **delivery is
not automatic**: with `--delivery-delay` unset a captured message stays at `sent` until
something advances it, so a test asserting "it was accepted" cannot race a ticker.

## Captured mail on disk

One `.eml` per message, in a directory per API key:

```
/data/<api-key>-<digest>/<message-id>.eml
```

That file is the record, not a side artifact. Open it in any mail client and you see
exactly what your code produced, headers and attachments included. The parts a mail message
has no header for — which key owns it, where it is in the lifecycle, its tags — ride along
in `X-Mock-Resend-*` headers, which are stripped on the way back in.

The directory name carries a digest of the key as well as the key itself, because two keys
that sanitise to the same filename would otherwise share a mailbox, and per-key isolation
is the thing the whole design rests on.

## The console

Open <http://localhost:9100> in a browser. There is no second port and nothing to start:
the console is embedded in the binary and served from the API listener under
`/_admin/console/`, with `/` redirecting there.

It lists every message captured, across every API key, newest first and updating while a
suite runs. Opening one gives you the switch Resend's own console has — **Preview**, **Plain
Text**, **HTML**, **Raw** — plus attachments you can download. Preview renders the HTML in a
fully sandboxed frame, because captured mail is whatever a caller sent and looking at a
message should not be able to do anything. Raw is the `.eml` byte for byte, which is usually
the fastest way to answer "why did that header not arrive". Each message also says how it
got here, `POST /emails` or `SMTP`.

It reads a JSON admin API on the same port, so anything it shows is scriptable:

```bash
curl -X POST http://localhost:9100/_admin/reset -d '{}'                    # wipe everything
curl -X POST http://localhost:9100/_admin/reset -d '{"api_key":"re_test"}' # wipe one key
curl "http://localhost:9100/_admin/emails?api_key=re_test"                 # what was captured
curl -X POST http://localhost:9100/_admin/clock -d '{"seconds":86400}'     # move time forward
```

Resetting a key is far faster than restarting the container between tests.

## Delivery events and webhooks

This is the part a mock that only stores mail cannot give you. A captured message moves
through the same lifecycle real Resend reports — `email.sent`, `email.delivered`,
`email.bounced`, `email.complained`, `email.delivery_delayed`, `email.failed`,
`email.opened`, `email.clicked` — and every transition is delivered to the webhook
endpoints registered for that key.

Webhooks are signed the way Resend signs them: `svix-id`, `svix-timestamp` and
`svix-signature`, HMAC-SHA256 over `{id}.{timestamp}.{body}`. Your real verification code
runs unmodified instead of being stubbed out, which is usually where the bugs are. A retry
reuses the `svix-id`, so a handler that deduplicates can be tested for it.

You do not have to wait for any of it. Per-key fault injection makes a send fail with a
given Resend error or a delivery bounce, an admin call moves a message to any event, and
the engine's clock can be advanced — so "this email goes out in 24 hours" is a test that
takes a millisecond rather than a day.

**Where webhooks may be sent is deliberately inverted** from a normal SSRF policy. Loopback
and private ranges are reachable, because that is where your handler is. Public internet
addresses are refused unless you pass `--allow-public-webhooks`, because a typo should not
post captured mail to a stranger. Link-local is refused either way, since `169.254.169.254`
is a cloud metadata service and CI is where this runs. Redirects are not followed.

## What works

Sending, batch sending with `x-batch-validation`, retrieve, update and cancel. `Bearer`
auth with per-key isolation, `Idempotency-Key` with the real 24-hour replay window, and
optional rate limiting with the `ratelimit-*` headers.

Validation matches Resend's, because validation is most of what a mock is for: the
50-recipient limit, the 100-message batch limit, the tag character set, `content` or `path`
on an attachment but not neither, and the string-or-array forms of `to`, `cc`, `bcc` and
`reply_to`. Errors come back with Resend's own `name` and status pairing in the
`{"statusCode", "message", "name"}` body a real SDK deserialises, so code that branches on
an error name can be tested against it.

SMTP on `:1025` is a second way into the same mailbox, not a second mailbox. Username
`resend`, password any API key — that is how the message finds its namespace. A message
that arrives over SMTP is retrievable through `GET /emails/{id}` and produces the same
events as one sent through the API. There is no `STARTTLS` and there will not be.

## What does not

Worth reading before you point a suite at this.

- **Nothing is authenticated and nothing is delivered.** Any `re_`-prefixed key is
  accepted, so a test asserting that a request is *refused* for a bad key will not get what
  it expects. And no message reaches a real inbox, ever — there is no MTA here.
- **Domains, API keys, audiences, contacts, broadcasts, topics and suppression** are out of
  scope. mock-resend exists so the code path that *sends* email can be tested without the
  network; every endpoint off that path is surface to keep faithful for no return.
- **Templates are not rendered.** A `template` id is accepted, validated and recorded, but
  nothing is rendered from it.
- **`scheduled_at` takes ISO 8601 only.** Resend also accepts natural language like
  `in 1 min`. An unparseable value is refused rather than treated as "now", because a
  scheduled send that goes out immediately is the kind of wrong answer a test would believe.
- **A batched message reports `last_event: "sent"`, not `"queued"`.** Matching Resend would
  mean an intermediate state the real API passes through in milliseconds, which a test
  asserting straight after a batch send would race.

Every deliberate divergence is listed in the full README, with the reasoning. The rule the
project is built on is that a divergence gets documented rather than plastered over: a mock
that is subtly wrong is worse than no mock, because it makes broken code pass.

## Links

- Source, full README and issues: [github.com/rapture-sh/mock-resend](https://github.com/rapture-sh/mock-resend)
- Security policy: [SECURITY.md](https://github.com/rapture-sh/mock-resend/blob/main/SECURITY.md)
- Licence: [MIT](https://github.com/rapture-sh/mock-resend/blob/main/LICENSE)

Part of [Rapture.sh](https://rapture.sh).

Resend is a trademark of Resend, Inc. This project is not affiliated with or endorsed by
Resend. "Resend-compatible" describes the HTTP API it implements, nothing more.
