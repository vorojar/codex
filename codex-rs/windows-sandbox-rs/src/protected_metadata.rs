use crate::setup::ProtectedMetadataMode;
use crate::setup::ProtectedMetadataTarget;
use anyhow::Context;
use anyhow::Result;
use std::collections::HashSet;
use std::fs::Metadata;
use std::io;
use std::os::windows::fs::FileTypeExt;
use std::os::windows::fs::MetadataExt;
use std::path::Path;
use std::path::PathBuf;
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

/// Layer: Windows enforcement layer. Existing metadata objects can be protected
/// with ACLs; missing names are monitored and removed if the sandbox creates
/// them.
#[derive(Debug)]
pub(crate) struct ProtectedMetadataGuard {
    deny_paths: Vec<PathBuf>,
    monitored_paths: Vec<PathBuf>,
}

impl ProtectedMetadataGuard {
    pub(crate) fn deny_paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.deny_paths.iter()
    }

    pub(crate) fn cleanup_created_monitored_paths(&self) -> Result<Vec<PathBuf>> {
        let mut removed = Vec::new();
        for path in &self.monitored_paths {
            let Some(existing_path) = existing_metadata_path(path)? else {
                continue;
            };
            remove_metadata_path(&existing_path)
                .with_context(|| format!("failed to remove protected metadata {}", path.display()))?;
            removed.push(existing_path);
        }
        Ok(removed)
    }
}

pub(crate) fn prepare_protected_metadata_targets(
    targets: &[ProtectedMetadataTarget],
) -> ProtectedMetadataGuard {
    let mut deny_paths = Vec::new();
    let mut monitored_paths = Vec::new();
    for target in targets {
        match target.mode {
            ProtectedMetadataMode::ExistingDeny => {
                deny_paths.extend(protected_metadata_existing_deny_paths(&target.path));
            }
            ProtectedMetadataMode::MissingCreationMonitor => {
                monitored_paths.push(target.path.clone());
            }
        }
    }
    ProtectedMetadataGuard {
        deny_paths,
        monitored_paths,
    }
}

pub fn protected_metadata_existing_deny_paths(path: &Path) -> Vec<PathBuf> {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return Vec::new();
    };

    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    push_deny_path(&mut paths, &mut seen, path.to_path_buf());

    let file_type = metadata.file_type();
    if (is_directory_reparse_point(&metadata)
        || file_type.is_symlink_dir()
        || file_type.is_symlink_file())
        && let Ok(target_path) = dunce::canonicalize(path)
    {
        push_deny_path(&mut paths, &mut seen, target_path);
    }

    paths
}

fn push_deny_path(paths: &mut Vec<PathBuf>, seen: &mut HashSet<String>, path: PathBuf) {
    if seen.insert(path_text_key(&path)) {
        paths.push(path);
    }
}

fn path_text_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn existing_metadata_path(path: &Path) -> Result<Option<PathBuf>> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => return Ok(Some(path.to_path_buf())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to inspect protected metadata {}", path.display()));
        }
    }

    let Some(parent) = path.parent() else {
        return Ok(None);
    };
    let Some(expected_name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to scan protected metadata parent {}", parent.display()));
        }
    };

    for entry in entries {
        let entry = entry.with_context(|| {
            format!(
                "failed to read protected metadata parent entry {}",
                parent.display()
            )
        })?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.eq_ignore_ascii_case(expected_name))
        {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

fn remove_metadata_path(path: &Path) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to inspect protected metadata {}", path.display()));
        }
    };
    let file_type = metadata.file_type();
    if is_directory_reparse_point(&metadata) || file_type.is_symlink_dir() {
        std::fs::remove_dir(path)
            .with_context(|| format!("failed to remove protected metadata {}", path.display()))?;
    } else if file_type.is_symlink_file() {
        std::fs::remove_file(path)
            .with_context(|| format!("failed to remove protected metadata {}", path.display()))?;
    } else if metadata.is_dir() {
        std::fs::remove_dir_all(path)
            .with_context(|| format!("failed to remove protected metadata {}", path.display()))?;
    } else {
        std::fs::remove_file(path)
            .with_context(|| format!("failed to remove protected metadata {}", path.display()))?;
    }
    Ok(())
}

fn is_directory_reparse_point(metadata: &Metadata) -> bool {
    metadata.is_dir() && (metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::ProtectedMetadataMode;
    use crate::setup::ProtectedMetadataTarget;

    #[test]
    fn cleanup_created_monitored_paths_removes_case_variant() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let target = temp_dir.path().join(".git");
        let created = temp_dir.path().join(".GIT");
        std::fs::create_dir_all(&created).expect("create metadata");
        let guard = prepare_protected_metadata_targets(&[ProtectedMetadataTarget {
            path: target.clone(),
            mode: ProtectedMetadataMode::MissingCreationMonitor,
        }]);

        let removed = guard.cleanup_created_monitored_paths().expect("cleanup");
        assert_eq!(removed.len(), 1);
        assert!(
            removed[0]
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case(".git")),
            "removed path should be a .git case variant: {}",
            removed[0].display()
        );
        assert!(!target.exists());
        assert!(!created.exists());
    }

    #[test]
    fn existing_deny_paths_include_symlink_target() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let target_dir = temp_dir.path().join("target-codex");
        let symlink_dir = temp_dir.path().join(".codex");
        std::fs::create_dir_all(&target_dir).expect("create target");
        if let Err(err) = std::os::windows::fs::symlink_dir(&target_dir, &symlink_dir) {
            eprintln!("skipping symlink test because symlink creation failed: {err}");
            return;
        }

        let guard = prepare_protected_metadata_targets(&[ProtectedMetadataTarget {
            path: symlink_dir.clone(),
            mode: ProtectedMetadataMode::ExistingDeny,
        }]);
        let deny_paths: Vec<PathBuf> = guard.deny_paths().cloned().collect();
        let canonical_target = dunce::canonicalize(&target_dir).expect("canonical target");

        assert!(
            deny_paths
                .iter()
                .any(|path| path_text_key(path) == path_text_key(&symlink_dir)),
            "deny paths should include metadata symlink: {deny_paths:?}"
        );
        assert!(
            deny_paths
                .iter()
                .any(|path| path_text_key(path) == path_text_key(&canonical_target)),
            "deny paths should include symlink target: {deny_paths:?}"
        );
    }
}
