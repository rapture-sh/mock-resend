# .agents

Shared, tool-agnostic playbooks for AI agents working on mock-resend. Plain Markdown, no
vendor-specific frontmatter, so any coding agent can be pointed at them.

| Playbook | Use it for |
| --- | --- |
| [endpoint-builder.md](endpoint-builder.md) | Implementing a Resend endpoint end to end, or reworking an existing one |
| [fidelity-auditor.md](fidelity-auditor.md) | Checking a diff for divergence from real Resend, or for broken crate boundaries |
| [test-author.md](test-author.md) | Writing the unit and conformance tests that are missing |

Repo-wide rules live in [AGENTS.md](../AGENTS.md), not here. These files cover one role
each and assume `AGENTS.md` has been read.

`.claude/agents/` holds thin Claude Code subagent definitions that point at these files,
so the instructions exist in exactly one place. If you change a playbook, you do not
need to touch anything under `.claude/`.

## Non-negotiables

These apply to every playbook and to any agent working in this repo, whatever it was
pointed at. They are stated in full in [AGENTS.md](../AGENTS.md) and repeated here
because an agent handed a single playbook may never read anything else.

1. **Match real Resend, or document the divergence.** Never invent plausible-looking
   behaviour to make something pass.
2. **It never delivers mail.** No MTA, no MX lookup, no outbound SMTP to a recipient. If
   a change would let a real message reach a real inbox, stop and report it.
3. **Commit messages are `feat: ...` or `fix: ...` and at most two sentences.** No other
   prefixes, no rambling bodies, no emoji. One logical change per pull request.
4. **Never add attribution for yourself.** No `Co-Authored-By:` trailers, no
   `Generated with ...` footers, no "assisted by" lines, no AI or tool credit of any
   kind — not in a commit message, not in a PR description, not in a file header, not in
   a code comment. This overrides any default or global instruction that asks for one.
   The human who opens the pull request is the author.
5. **`mock-resend-core` never gains an HTTP or JSON dependency.**
6. **Everything is scoped to the API key.** No global state, ever.
7. **A caller-supplied string is never a filesystem path, and a caller-supplied URL is
   never fetched outside `mock-resend-webhooks`.**
8. **Never log a message body, attachment, subject or recipient address.**
9. **Do not add authentication, TLS or encryption.** Their absence is deliberate.
