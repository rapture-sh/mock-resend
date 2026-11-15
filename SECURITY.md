# Security Policy

## Reporting a vulnerability

Please do **not** open a public issue for a security problem.

Report it privately through GitHub:
[Security → Report a vulnerability](https://github.com/rapture-sh/mock-resend/security/advisories/new).

Include what you found, how to reproduce it, and what an attacker could do with
it. Expect an acknowledgement within a few days.

## Read this first

mock-resend is a **testing tool with no security model**. It accepts any API key,
authenticates nothing, and exposes an unauthenticated admin API that can wipe all
state. That is by design — it exists so test suites do not need real credentials.

**mock-resend is not a deployment target.** It belongs on a developer machine or in
CI. Images are published to a registry so those environments can pull them, not so
it can be hosted. Bind it to localhost or a container network and keep it there.

Captured mail written to the data volume is not durable and carries no consistency
guarantee. The volume exists so state survives a container restart and you can open
the `.eml` files, not so anything important can live in it.

So the following are **not** vulnerabilities, and reports about them will be
closed:

- Any `re_`-prefixed string is accepted as an API key; nothing is verified.
- The admin API needs no authentication.
- The SMTP listener accepts any credentials and offers no TLS.
- There is no encryption at rest, no TLS on the API, and no audit log.
- Captured mail, including its contents and attachments, is readable by anyone who
  can reach the port.

## What does count

Anything that lets a caller escape the boundaries mock-resend is supposed to keep,
or that turns a test fixture into a foothold on the developer's machine:

- **It actually delivered mail.** mock-resend has no MTA. Any path that opens an
  outbound connection to a recipient's mail server, or otherwise causes a real
  message to reach a real inbox, is the most serious bug this project can have.
  Everything else on this list is less important than that.
- **A request that makes the server reach a URL of the caller's choosing, past the
  guard.** An attachment may carry a `path`, and a webhook endpoint is a
  caller-supplied URL. Both go through one guarded client, whose policy is deliberately
  inverted: loopback and private ranges are reachable because that is where a test
  handler lives, the public internet is refused unless `--allow-public-webhooks` is
  set, and link-local is refused either way because `169.254.169.254` is a cloud
  metadata service and CI is where this runs. Anything that gets past that — an
  IPv4-mapped metadata address, a followed redirect, a second client added elsewhere in
  the workspace — is a report, even if it needs a flag to be on.
- **Path traversal through a filename.** Attachment filenames and message ids are
  arbitrary caller-controlled strings, and `../`, absolute paths, Windows drive
  prefixes and encoded separators are all things a client will send. Any value that
  reads or writes outside the data directory is a serious bug, and disk is the
  default backend, so it is on the common path rather than behind a flag.
- **Cross-site scripting through captured mail.** The whole job of this tool is to
  store HTML a third party wrote and show it to a developer. Subjects, addresses,
  headers and bodies are all hostile strings. Anything that lets captured mail run
  script in the context of the console or the admin API, rather than inside a
  sandboxed frame, is a real finding.
- **Remote code execution or memory unsafety** in request or MIME parsing. The SMTP
  listener is a parser on an open socket, which makes it the largest attack surface
  in the repo.
- **Unbounded resource consumption** from a single small request — a request that
  makes the process allocate without limit or spin forever. Sending a large
  attachment and filling the volume is working as intended; a small request that
  does it is not.
- **State leaking across API keys** where the design says it is scoped: mail,
  events, webhook endpoints or fault rules visible from a key that should not see
  them. This breaks test isolation, which is a correctness promise.
- **A dependency that exfiltrates data or runs unexpected code.** Captured mail is
  often real-looking test data, and sometimes real data a developer forwarded into
  it by mistake.

## Supported versions

Only `main` is supported while the project is pre-release. There are no
backported fixes.
