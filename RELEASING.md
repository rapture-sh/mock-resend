# Releasing

For maintainers. Everything here is driven from a tag; nothing is published by hand.

## Before tagging

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --workspace
```

Then check the three things CI cannot:

- **`README.md` matches reality.** The supported-surface and configuration tables carry a
  landed marker per row. A row that says a thing works has to actually work, and a
  divergence that is still there has to still be listed under Known divergences.
- **The version in `Cargo.toml` is the version you are about to tag.** It is a workspace
  field, so every crate moves together.
- **`cargo xtask docker` builds and the image starts.** CI does this on every pull
  request, so it is a formality, but the release workflow pushes what it builds.

## Tagging

```bash
git tag -a v0.1.0 -m "v0.1.0" && git push origin v0.1.0
```

`v`-prefixed tags are what the release workflow watches. Pushing one builds the
container image for `linux/amd64` and `linux/arm64`, pushes it to
`ghcr.io/rapture-sh/mock-resend` as both `:0.1.0` and `:latest`, and opens a draft
GitHub release.

The draft is deliberate: write the notes, then publish it.

## What `:latest` means

The most recent tag, not the tip of `main`. Someone's CI pins `:latest` and expects it to
keep working, so nothing reaches that tag without a version number attached to it.

## Versioning

While the major is `0`, the minor is where breaking changes go.

Two things are specifically allowed to break in a minor release, and the README says so:
the `/_admin` surface, and anything listed as a divergence from real Resend. The Resend
surface itself is the thing that has to stay still — code pointing at it is pointing at
an API someone else designed, and this project does not get to change it.

## If a release is wrong

Do not move or delete a published tag. Someone has already pulled it. Fix forward with
the next patch version, and if the image is actively harmful, the registry can retract
a tag separately.
