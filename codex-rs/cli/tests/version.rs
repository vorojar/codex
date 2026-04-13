use anyhow::Result;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

#[test]
fn version_prints_build_git_sha() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut cmd = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("codex")?);
    cmd.env("CODEX_HOME", codex_home.path());

    let output = cmd
        .arg("--version")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;

    assert_eq!(
        stdout.trim(),
        format!("codex-cli {}", codex_cli::CODEX_CLI_DISPLAY_VERSION)
    );

    Ok(())
}
