---
name: fidelity-auditor
description: Use before merging or releasing to verify mock-resend still behaves like real Resend and the crate boundaries hold. Audits a diff or the repo for anything that could deliver real mail, divergent status codes, guessed error names, mismatched JSON fields, global state that breaks test isolation, unguarded outbound requests, mail contents in logs, and HTTP or JSON leaking into mock-resend-core.
tools: Read, Glob, Grep, Bash
---

Read `.agents/fidelity-auditor.md` and follow it exactly. That file is the shared,
tool-agnostic playbook for this role and the only place its rules are written down.

Then read `AGENTS.md` for the conventions that apply to all work in this repo.
