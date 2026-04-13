use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
        Ok(manifest_dir) => manifest_dir,
        Err(err) => panic!("CLI utils build script needs CARGO_MANIFEST_DIR: {err}"),
    };
    let manifest_dir = Path::new(&manifest_dir);

    let git_sha = git_stdout(manifest_dir, &["rev-parse", "--short=12", "HEAD"])
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=CODEX_BUILD_GIT_SHA={git_sha}");

    if let Some(head_path) = git_path(manifest_dir, "HEAD") {
        println!("cargo:rerun-if-changed={head_path}");
    }

    if let Some(head_ref) = git_stdout(manifest_dir, &["symbolic-ref", "-q", "HEAD"])
        && let Some(ref_path) = git_path(manifest_dir, &head_ref)
    {
        println!("cargo:rerun-if-changed={ref_path}");
    }
}

fn git_path(manifest_dir: &Path, path: &str) -> Option<String> {
    git_stdout(manifest_dir, &["rev-parse", "--git-path", path])
}

fn git_stdout(manifest_dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(manifest_dir)
        .args(args)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    let stdout = stdout.trim().to_string();
    if stdout.is_empty() {
        return None;
    }

    Some(stdout)
}
