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

`v`-prefixed tags are what the release workflow watches. Pushing one:

1. Builds the image on a machine of each architecture — `linux/amd64` and `linux/arm64`,
   natively, not under QEMU — and pushes each by digest, with no tag attached.
2. **Runs each image before anything is tagged.** Liveness, a message captured through
   the API and read back by its id, a message captured over SMTP, and the console's
   `<title>`, which is how a missing `web/dist` gets caught. A build that fails this
   never becomes `:latest`.
3. Stitches the two digests into one multi-architecture index and attaches the tags to
   it: `0.1.0`, `0.1`, `0` and `latest`, so someone can pin exactly, follow patches,
   follow minors, or just take the newest.
4. Checks both architectures are actually in the published index.
5. Updates the Docker Hub description from `README.md`, which is otherwise blank and is
   the first thing anyone sees.
6. Opens a draft GitHub release.

The tags are attached in one step at the end, so there is never a moment where `:latest`
means one architecture. The release build uses no cache, deliberately: a shared cache is
a way for a poisoned entry to reach a published image, and a native build is a couple of
minutes anyway.

The draft is deliberate: write the notes, then publish it. The image is already out by
that point, which is the right order — the notes point at something that exists.

It needs two secrets on the repository, `DOCKERHUB_USERNAME` and `DOCKERHUB_TOKEN`, and
the Blacksmith GitHub App for the native ARM runner. Without the app those jobs queue
rather than fail; `release.yaml` says what to swap the labels for.

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
