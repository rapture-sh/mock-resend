//! Build orchestration, as a plain Rust binary so no extra tool is needed on any
//! platform. Run it through the cargo alias: `cargo xtask <task>`.
//!
//! Deliberately has no dependencies: it should compile in a second.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const IMAGE: &str = "raptureoss/mock-resend";

fn main() -> ExitCode {
    let task = std::env::args().nth(1);

    let result = match task.as_deref() {
        Some("console") => console(),
        Some("dist") => dist(),
        Some("docker") => docker(),
        Some(other) => Err(format!("unknown task: {other}\n\n{}", usage())),
        None => Err(usage()),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> String {
    format!(
        "tasks:\n  \
         console   build the web console into web/dist\n  \
         dist      build the console, then the release binary\n  \
         docker    build the container image ({IMAGE}:dev)\n"
    )
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is xtask/, so the root is one level up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask is never the workspace root")
        .to_path_buf()
}

fn run(program: &str, args: &[&str], cwd: &Path) -> Result<(), String> {
    println!("==> {program} {}", args.join(" "));

    let status = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .status()
        .map_err(|error| format!("could not start {program}: {error}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} exited with {status}"))
    }
}

/// Build the console's assets.
///
/// `rust-embed` compiles `web/dist` into the binary, so this has to happen first or the
/// release binary ships whatever was there last — or, in a fresh checkout, a page saying
/// the console was never built.
fn console() -> Result<(), String> {
    let web = workspace_root().join("web");

    if !web.join("package.json").exists() {
        return Err("no web/package.json at the workspace root".to_owned());
    }

    let pnpm = if cfg!(windows) { "pnpm.cmd" } else { "pnpm" };

    run(pnpm, &["install", "--frozen-lockfile"], &web)?;
    run(pnpm, &["run", "build"], &web)
}

fn dist() -> Result<(), String> {
    console()?;

    run("cargo", &["build", "--release"], &workspace_root())
}

fn docker() -> Result<(), String> {
    let root = workspace_root();

    // The `Dockerfile` is part of the repository now, so its absence means a broken
    // checkout rather than a phase that has not landed yet.
    if !root.join("Dockerfile").exists() {
        return Err("no Dockerfile at the workspace root".to_owned());
    }

    run(
        "docker",
        &["build", "-t", &format!("{IMAGE}:dev"), "."],
        &root,
    )
}
