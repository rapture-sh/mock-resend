---
description: Run the full local check suite
---

Run these in order and report what fails, with the relevant output:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
```

Fix anything mechanical (formatting, import order, an obvious clippy suggestion)
yourself; `cargo fmt --all` handles the first category. For real failures, explain the
cause before changing behaviour — and if a conformance test fails, assume mock-resend is
wrong about Resend before assuming the test is.

If a test fails intermittently, do not retry it until it passes. Look for a sleep, a
hardcoded port, or two tests sharing an API key; those are the three ways a test in this
repo becomes flaky, and all three are bugs in the test.
