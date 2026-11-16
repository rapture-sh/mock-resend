## What this changes

<!-- One or two sentences. Link the issue if there is one. -->

## Why

<!-- What problem does it solve? For a fidelity fix: what does real Resend do, and how do
you know — a doc link, an SDK type, or an observed response. -->

## Checklist

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] Console changes also pass `pnpm lint`, `pnpm typecheck` and `pnpm build` in `web/`
- [ ] Commit messages are `feat: ...` or `fix: ...`, two sentences at most
- [ ] Any new dependency is named below with a reason the standard library or an existing
      one cannot cover it

## If this adds or changes API surface

<!-- Delete this section if it does not. -->

- [ ] Matches real Resend — same status, same error `name`, same JSON shape, same field
      names, same headers — or the divergence is documented under Known divergences in
      `README.md` and says why
- [ ] A conformance test in `crates/conformance/tests/` drives it through a real Resend
      SDK, not hand-written HTTP alone
- [ ] A bug fix has a regression test that fails without the fix
- [ ] The supported-surface table in `README.md` is up to date
- [ ] `mock-resend-core` gained no HTTP or JSON dependency
- [ ] Nothing new is scoped globally; it belongs to an API key
- [ ] No caller-supplied string became a path except through `core::store::resolve_in`,
      and no outbound request was added outside `mock-resend-webhooks`
- [ ] Nothing logs a message body, subject, recipient or attachment

## New dependencies

<!-- None, or: the crate, what it does, why the standard library or a settled dependency
cannot, and what it costs in compile time. -->

## Performance

<!-- Delete unless this claims to be faster. Numbers before and after, and how you
measured. -->
