use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs::File;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;
use tempfile::NamedTempFile;

/// Environment variable used to hand the private fork-env snapshot path to a
/// child Codex process launched in a new iTerm tab.
pub const FORK_ENV_SNAPSHOT_PATH_ENV_VAR: &str = "CODEX_FORK_ENV_SNAPSHOT_PATH";
const FORK_ENV_SNAPSHOT_DIR: &str = "fork-env";
const FORK_ENV_SNAPSHOT_PREFIX: &str = "fork-env-";
const FORK_ENV_SNAPSHOT_STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
const FORK_ENV_SNAPSHOT_VERSION: u16 = 1;
const DROP_IMPORTED_ENV_VARS: &[&str] = &["CODEX_THREAD_ID"];
const PRESERVE_CURRENT_ENV_VARS: &[&str] = &[
    "ITERM_SESSION_ID",
    "ITERM_PROFILE",
    "ITERM_PROFILE_NAME",
    "OLDPWD",
    "PWD",
    "SHLVL",
];

#[cfg(unix)]
type EncodedEnvComponent = Vec<u8>;
#[cfg(windows)]
type EncodedEnvComponent = Vec<u16>;

#[derive(Debug, Serialize, Deserialize)]
struct ForkEnvSnapshot {
    version: u16,
    entries: Vec<ForkEnvEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ForkEnvEntry {
    key: EncodedEnvComponent,
    value: EncodedEnvComponent,
}

/// A private environment snapshot that the parent process keeps alive until the
/// fork launch succeeds.
pub struct ForkEnvSnapshotFile {
    file: Option<NamedTempFile>,
}

impl ForkEnvSnapshotFile {
    /// Snapshot the current process environment into a private temp file.
    pub fn create_for_current_process(codex_home: &Path) -> Result<Self> {
        let snapshot_root = ensure_snapshot_root(codex_home)?;
        cleanup_stale_fork_env_snapshots(&snapshot_root)?;

        let mut file = tempfile::Builder::new()
            .prefix(FORK_ENV_SNAPSHOT_PREFIX)
            .rand_bytes(6)
            .tempfile_in(&snapshot_root)
            .with_context(|| {
                format!(
                    "failed to create fork environment snapshot in {}",
                    snapshot_root.display()
                )
            })?;

        #[cfg(unix)]
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o600))
            .with_context(|| {
                format!(
                    "failed to restrict fork environment snapshot permissions for {}",
                    file.path().display()
                )
            })?;

        write_snapshot_file(file.as_file_mut(), std::env::vars_os()).with_context(|| {
            format!(
                "failed to write fork environment snapshot {}",
                file.path().display()
            )
        })?;
        file.as_file_mut().flush().with_context(|| {
            format!(
                "failed to flush fork environment snapshot {}",
                file.path().display()
            )
        })?;

        Ok(Self { file: Some(file) })
    }

    /// Return the snapshot path to pass to the child process.
    pub fn path(&self) -> Result<&Path> {
        self.file
            .as_ref()
            .map(NamedTempFile::path)
            .context("fork snapshot path requested after persistence")
    }

    /// Hand ownership of the snapshot file to the child process.
    pub fn persist_for_child(mut self) -> std::io::Result<()> {
        if let Some(file) = self.file.take() {
            let (_persisted_file, _persisted_path) = file.keep()?;
        }
        Ok(())
    }
}

/// Import the parent environment snapshot when the child was launched via the
/// iTerm fork flow. Returns `Ok(true)` when a snapshot was consumed.
pub(crate) fn import_fork_env_from_snapshot_path_env() -> Result<bool> {
    let Some(snapshot_path) = std::env::var_os(FORK_ENV_SNAPSHOT_PATH_ENV_VAR) else {
        return Ok(false);
    };
    let snapshot_path = PathBuf::from(snapshot_path);
    // One-shot startup contract: once the child has consumed the path, do not
    // let descendant processes inherit the handoff pointer.
    unsafe {
        std::env::remove_var(FORK_ENV_SNAPSHOT_PATH_ENV_VAR);
    }

    let current_env = std::env::vars_os().collect::<Vec<_>>();
    let snapshot = read_snapshot_file_and_delete(snapshot_path.as_path()).with_context(|| {
        format!(
            "failed to import fork environment snapshot from {}",
            snapshot_path.display()
        )
    })?;
    let imported_env = build_imported_env(snapshot, current_env)?;
    replace_process_env(imported_env);
    Ok(true)
}

fn ensure_snapshot_root(codex_home: &Path) -> Result<PathBuf> {
    let root = codex_home.join("tmp").join(FORK_ENV_SNAPSHOT_DIR);
    std::fs::create_dir_all(&root).with_context(|| {
        format!(
            "failed to create fork environment snapshot directory {}",
            root.display()
        )
    })?;

    #[cfg(unix)]
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).with_context(|| {
        format!(
            "failed to restrict fork environment snapshot directory permissions for {}",
            root.display()
        )
    })?;

    Ok(root)
}

fn cleanup_stale_fork_env_snapshots(snapshot_root: &Path) -> Result<()> {
    let entries = match std::fs::read_dir(snapshot_root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        if !file_name.starts_with(FORK_ENV_SNAPSHOT_PREFIX) {
            continue;
        }

        let is_stale = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age > FORK_ENV_SNAPSHOT_STALE_AFTER);
        if is_stale {
            let _ = std::fs::remove_file(path);
        }
    }

    Ok(())
}

fn write_snapshot_file<I>(writer: &mut File, vars: I) -> Result<()>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let snapshot = ForkEnvSnapshot {
        version: FORK_ENV_SNAPSHOT_VERSION,
        entries: vars
            .into_iter()
            .map(|(key, value)| ForkEnvEntry {
                key: encode_os_string(key.as_os_str()),
                value: encode_os_string(value.as_os_str()),
            })
            .collect(),
    };
    bincode::serialize_into(writer, &snapshot)
        .context("failed to serialize fork environment snapshot")
}

fn read_snapshot_file_and_delete(snapshot_path: &Path) -> Result<ForkEnvSnapshot> {
    let mut file = File::open(snapshot_path).with_context(|| {
        format!(
            "failed to open fork environment snapshot {}",
            snapshot_path.display()
        )
    })?;

    #[cfg(unix)]
    std::fs::remove_file(snapshot_path)
        .or_else(ignore_not_found)
        .with_context(|| {
            format!(
                "failed to delete fork environment snapshot {} after opening it",
                snapshot_path.display()
            )
        })?;

    let snapshot: ForkEnvSnapshot = bincode::deserialize_from(&mut file)
        .context("failed to decode fork environment snapshot")?;

    #[cfg(windows)]
    std::fs::remove_file(snapshot_path)
        .or_else(ignore_not_found)
        .with_context(|| {
            format!(
                "failed to delete fork environment snapshot {} after reading it",
                snapshot_path.display()
            )
        })?;

    if snapshot.version != FORK_ENV_SNAPSHOT_VERSION {
        anyhow::bail!(
            "unsupported fork environment snapshot version {}",
            snapshot.version
        );
    }

    Ok(snapshot)
}

fn ignore_not_found(err: std::io::Error) -> std::io::Result<()> {
    if err.kind() == std::io::ErrorKind::NotFound {
        Ok(())
    } else {
        Err(err)
    }
}

fn build_imported_env<I>(
    snapshot: ForkEnvSnapshot,
    current_env: I,
) -> Result<HashMap<OsString, OsString>>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let mut imported_env = snapshot
        .entries
        .into_iter()
        .map(|entry| {
            Ok::<(OsString, OsString), anyhow::Error>((
                decode_os_string(entry.key),
                decode_os_string(entry.value),
            ))
        })
        .collect::<Result<HashMap<_, _>>>()?;
    let current_env = current_env.into_iter().collect::<HashMap<_, _>>();

    strip_parent_arg0_path_entry(&mut imported_env)?;

    for name in DROP_IMPORTED_ENV_VARS {
        remove_named_env(&mut imported_env, name);
    }

    for name in PRESERVE_CURRENT_ENV_VARS {
        if let Some(current_value) = get_named_env(&current_env, name) {
            set_named_env(&mut imported_env, name, current_value.clone());
        } else {
            remove_named_env(&mut imported_env, name);
        }
    }

    Ok(imported_env)
}

fn replace_process_env(imported_env: HashMap<OsString, OsString>) {
    let existing_keys = std::env::vars_os()
        .map(|(key, _value)| key)
        .collect::<Vec<_>>();
    for key in existing_keys {
        unsafe {
            std::env::remove_var(key);
        }
    }
    for (key, value) in imported_env {
        unsafe {
            std::env::set_var(key, value);
        }
    }
}

fn strip_parent_arg0_path_entry(imported_env: &mut HashMap<OsString, OsString>) -> Result<()> {
    let Some(path_value) = get_named_env(imported_env, "PATH") else {
        return Ok(());
    };
    let Some(codex_home) = infer_codex_home(imported_env) else {
        return Ok(());
    };
    let arg0_root = codex_home.join("tmp").join("arg0");
    let filtered_paths = std::env::split_paths(&path_value)
        .filter(|entry| !entry.starts_with(&arg0_root))
        .collect::<Vec<_>>();
    let filtered_path = std::env::join_paths(filtered_paths)
        .context("failed to rebuild PATH while stripping inherited arg0 helpers")?;
    set_named_env(imported_env, "PATH", filtered_path);
    Ok(())
}

fn infer_codex_home(imported_env: &HashMap<OsString, OsString>) -> Option<PathBuf> {
    if let Some(codex_home) = get_named_env(imported_env, "CODEX_HOME")
        && !codex_home.is_empty()
    {
        return Some(PathBuf::from(codex_home));
    }

    if let Some(home) = get_named_env(imported_env, "HOME")
        && !home.is_empty()
    {
        return Some(PathBuf::from(home).join(".codex"));
    }

    #[cfg(windows)]
    if let Some(home) = get_named_env(imported_env, "USERPROFILE")
        && !home.is_empty()
    {
        return Some(PathBuf::from(home).join(".codex"));
    }

    None
}

fn get_named_env<'a>(env_map: &'a HashMap<OsString, OsString>, name: &str) -> Option<&'a OsString> {
    env_map
        .iter()
        .find(|(candidate, _value)| env_name_matches(candidate.as_os_str(), name))
        .map(|(_candidate, value)| value)
}

fn set_named_env(env_map: &mut HashMap<OsString, OsString>, name: &str, value: OsString) {
    remove_named_env(env_map, name);
    env_map.insert(OsString::from(name), value);
}

fn remove_named_env(env_map: &mut HashMap<OsString, OsString>, name: &str) {
    if let Some(existing_key) = env_map
        .keys()
        .find(|candidate| env_name_matches(candidate.as_os_str(), name))
        .cloned()
    {
        env_map.remove(&existing_key);
    }
}

fn env_name_matches(candidate: &OsStr, name: &str) -> bool {
    #[cfg(windows)]
    {
        candidate.to_string_lossy().eq_ignore_ascii_case(name)
    }
    #[cfg(not(windows))]
    {
        candidate == OsStr::new(name)
    }
}

#[cfg(unix)]
fn encode_os_string(value: &OsStr) -> EncodedEnvComponent {
    value.as_bytes().to_vec()
}

#[cfg(unix)]
fn decode_os_string(value: EncodedEnvComponent) -> OsString {
    OsString::from_vec(value)
}

#[cfg(windows)]
fn encode_os_string(value: &OsStr) -> EncodedEnvComponent {
    use std::os::windows::ffi::OsStrExt;

    value.encode_wide().collect()
}

#[cfg(windows)]
fn decode_os_string(value: EncodedEnvComponent) -> OsString {
    use std::os::windows::ffi::OsStringExt;

    OsString::from_wide(&value)
}

#[cfg(test)]
mod tests {
    use super::FORK_ENV_SNAPSHOT_DIR;
    use super::FORK_ENV_SNAPSHOT_PATH_ENV_VAR;
    use super::ForkEnvSnapshot;
    use super::ForkEnvSnapshotFile;
    use super::build_imported_env;
    use super::encode_os_string;
    use super::import_fork_env_from_snapshot_path_env;
    use super::read_snapshot_file_and_delete;
    use super::write_snapshot_file;
    use pretty_assertions::assert_eq;
    use serial_test::serial;
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;
    use tempfile::NamedTempFile;
    use tempfile::TempDir;

    fn snapshot_from_env(vars: Vec<(OsString, OsString)>) -> ForkEnvSnapshot {
        let mut file = NamedTempFile::new().expect("temp snapshot file");
        write_snapshot_file(file.as_file_mut(), vars).expect("write snapshot");
        let mut reopened = std::fs::File::open(file.path()).expect("reopen snapshot");
        bincode::deserialize_from(&mut reopened).expect("decode snapshot")
    }

    fn env_map(entries: &[(&str, &str)]) -> HashMap<OsString, OsString> {
        entries
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect()
    }

    struct ProcessEnvGuard {
        original: Vec<(OsString, OsString)>,
    }

    impl ProcessEnvGuard {
        fn capture() -> Self {
            Self {
                original: std::env::vars_os().collect(),
            }
        }
    }

    impl Drop for ProcessEnvGuard {
        fn drop(&mut self) {
            let existing_keys = std::env::vars_os()
                .map(|(key, _value)| key)
                .collect::<Vec<_>>();
            unsafe {
                for key in existing_keys {
                    std::env::remove_var(key);
                }
                for (key, value) in &self.original {
                    std::env::set_var(key, value);
                }
            }
        }
    }

    #[serial]
    #[test]
    #[cfg(unix)]
    fn snapshot_bincode_roundtrip_preserves_non_utf8_env_values() {
        let env_vars = vec![(
            OsString::from_vec(b"PATH".to_vec()),
            OsString::from_vec(b"/tmp/\xff/bin".to_vec()),
        )];
        let snapshot = snapshot_from_env(env_vars.clone());

        assert_eq!(snapshot.version, 1);
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(
            snapshot.entries[0].key,
            encode_os_string(env_vars[0].0.as_os_str())
        );
        assert_eq!(
            snapshot.entries[0].value,
            encode_os_string(env_vars[0].1.as_os_str())
        );
    }

    #[serial]
    #[test]
    fn build_imported_env_preserves_current_session_values_and_strips_parent_arg0_path_entry() {
        let snapshot = snapshot_from_env(vec![
            (
                OsString::from("PATH"),
                OsString::from("/tmp/other:/tmp/home/.codex/tmp/arg0/codex-arg0parent:/bin"),
            ),
            (
                OsString::from("ITERM_SESSION_ID"),
                OsString::from("parent-session"),
            ),
            (OsString::from("PWD"), OsString::from("/parent/pwd")),
            (OsString::from("SHLVL"), OsString::from("7")),
            (
                OsString::from("CODEX_THREAD_ID"),
                OsString::from("parent-thread-id"),
            ),
            (OsString::from("HOME"), OsString::from("/tmp/home")),
        ]);
        let imported = build_imported_env(
            snapshot,
            env_map(&[
                ("ITERM_SESSION_ID", "child-session"),
                ("PWD", "/child/pwd"),
                ("SHLVL", "2"),
            ]),
        )
        .expect("imported env");

        assert_eq!(
            imported.get(&OsString::from("PATH")),
            Some(&OsString::from("/tmp/other:/bin"))
        );
        assert_eq!(
            imported.get(&OsString::from("ITERM_SESSION_ID")),
            Some(&OsString::from("child-session"))
        );
        assert_eq!(
            imported.get(&OsString::from("PWD")),
            Some(&OsString::from("/child/pwd"))
        );
        assert_eq!(
            imported.get(&OsString::from("SHLVL")),
            Some(&OsString::from("2"))
        );
        assert_eq!(imported.get(&OsString::from("CODEX_THREAD_ID")), None);
    }

    #[serial]
    #[test]
    fn create_for_current_process_writes_snapshot_under_codex_home_tmp() {
        let codex_home = TempDir::new().expect("codex home");
        let snapshot =
            ForkEnvSnapshotFile::create_for_current_process(codex_home.path()).expect("snapshot");
        let expected_parent = codex_home.path().join("tmp").join(FORK_ENV_SNAPSHOT_DIR);
        let snapshot_path = snapshot.path().unwrap();

        assert_eq!(snapshot_path.parent(), Some(expected_parent.as_path()));
        assert!(snapshot_path.exists());
    }

    #[serial]
    #[test]
    fn import_fork_env_from_snapshot_path_env_replaces_process_env() {
        let _env = ProcessEnvGuard::capture();
        let snapshot_dir = TempDir::new().expect("snapshot dir");
        let mut snapshot_file =
            NamedTempFile::new_in(snapshot_dir.path()).expect("fork snapshot file");
        write_snapshot_file(
            snapshot_file.as_file_mut(),
            vec![
                (OsString::from("HOME"), OsString::from("/snapshot/home")),
                (OsString::from("PATH"), OsString::from("/snapshot/bin")),
                (
                    OsString::from("ITERM_SESSION_ID"),
                    OsString::from("parent-session"),
                ),
                (OsString::from("PWD"), OsString::from("/snapshot/pwd")),
            ],
        )
        .expect("write snapshot");
        snapshot_file.as_file_mut().flush().expect("flush snapshot");

        let snapshot_path = snapshot_file
            .into_temp_path()
            .keep()
            .expect("persist snapshot");
        unsafe {
            std::env::set_var(
                FORK_ENV_SNAPSHOT_PATH_ENV_VAR,
                snapshot_path.to_string_lossy().as_ref(),
            );
            std::env::set_var("HOME", "/child/home");
            std::env::set_var("PATH", "/child/bin");
            std::env::set_var("ITERM_SESSION_ID", "child-session");
            std::env::set_var("PWD", "/child/pwd");
            std::env::set_var("EXTRA_CURRENT_ONLY", "extra");
        }

        let imported = import_fork_env_from_snapshot_path_env().expect("import fork env");

        assert_eq!(imported, true);
        assert_eq!(
            std::env::var_os("HOME"),
            Some(OsString::from("/snapshot/home"))
        );
        assert_eq!(
            std::env::var_os("PATH"),
            Some(OsString::from("/snapshot/bin"))
        );
        assert_eq!(
            std::env::var_os("ITERM_SESSION_ID"),
            Some(OsString::from("child-session"))
        );
        assert_eq!(std::env::var_os("PWD"), Some(OsString::from("/child/pwd")));
        assert_eq!(std::env::var_os("EXTRA_CURRENT_ONLY"), None);
        assert_eq!(std::env::var_os(FORK_ENV_SNAPSHOT_PATH_ENV_VAR), None);
        assert!(!snapshot_path.exists());
    }

    #[serial]
    #[test]
    #[cfg(unix)]
    fn read_snapshot_file_and_delete_unlinks_before_decode_failure() {
        let snapshot_dir = TempDir::new().expect("snapshot dir");
        let snapshot_path = snapshot_dir.path().join("fork-env-corrupt.bin");
        std::fs::write(&snapshot_path, b"not a valid bincode snapshot").expect("write snapshot");

        let err = read_snapshot_file_and_delete(&snapshot_path).expect_err("decode should fail");

        assert!(
            err.to_string()
                .contains("failed to decode fork environment snapshot"),
            "unexpected error: {err:#}"
        );
        assert!(!snapshot_path.exists());
    }

    #[serial]
    #[test]
    fn import_fork_env_from_snapshot_path_env_returns_false_when_unset() {
        let _env = ProcessEnvGuard::capture();
        unsafe {
            std::env::remove_var(FORK_ENV_SNAPSHOT_PATH_ENV_VAR);
        }

        let imported = import_fork_env_from_snapshot_path_env().expect("import fork env");

        assert_eq!(imported, false);
    }
}
