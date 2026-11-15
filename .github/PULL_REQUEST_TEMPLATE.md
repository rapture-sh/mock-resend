## What this changes

<!-- One or two sentences. Link the issue if there is one. -->

## Why

<!-- What problem does it solve? For a fidelity fix: what does real Resend do, and
how do you know — a doc link or an observed response. -->

## Checklist

- [ ] Behaviour matches real Resend, or the divergence is documented in the README
- [ ] `mock-resend-core` still has no HTTP or JSON dependencies
- [ ] Nothing new is scoped globally; it belongs to an API key
- [ ] `cargo fmt --all --check` passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] New API surface has a conformance test driven through a real Resend SDK
- [ ] Bug fixes have a regression test
- [ ] Supported-surface table in the README updated, if this adds or changes one
- [ ] Commit messages are `feat: ...` or `fix: ...`, two sentences at most

## New dependencies

<!-- None, or: the crate, what it does, why the standard library or an existing
dependency can't, and what it costs in compile time. -->

## Performance

<!-- Delete unless this claims to be faster. Numbers before and after, and how
you measured. -->
