//! Makes sure `web/dist` exists before `rust-embed` looks for it.
//!
//! `rust-embed` resolves its `#[folder]` when the derive expands, so a missing directory
//! is a *compile* error — and `web/dist` is a build artifact that is not in the
//! repository. Without this, the whole workspace would fail to build for anyone who had
//! not run `pnpm --dir web build` first, which is a confusing way to meet a project.
//!
//! A tracked `.gitkeep` would have done the same job, except that Vite's `emptyOutDir`
//! deletes it on every console build: the first person to build the console and commit
//! would quietly remove the placeholder and break the build for everyone else. Creating
//! the directory here cannot be undone by a build.
//!
//! An empty directory is not a broken console. `console::index` serves a page explaining
//! how to build it, and the API works either way.

use std::path::Path;

fn main() {
    // Relative to this crate's manifest, which is where Cargo runs a build script.
    let dist = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("web")
        .join("dist");

    // A failure here is not fatal: if the directory cannot be created, the derive below
    // will say so far more clearly than a build script can.
    let _created = std::fs::create_dir_all(&dist);

    // Rerun when the assets change, so a `pnpm build` is picked up without a
    // `cargo clean`. Without this, Cargo would cache the embedded assets from the last
    // compile and a rebuilt console would not appear in the binary.
    println!("cargo:rerun-if-changed={}", dist.display());
}
