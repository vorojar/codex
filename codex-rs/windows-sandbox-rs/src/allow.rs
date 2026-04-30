use crate::policy::SandboxPolicy;
use codex_utils_absolute_path::AbsolutePathBuf;
use dunce::canonicalize;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct AllowDenyPaths {
    pub allow: HashSet<PathBuf>,
    pub deny: HashSet<PathBuf>,
}

pub fn compute_allow_paths(
    policy: &SandboxPolicy,
    policy_cwd: &Path,
    command_cwd: &Path,
    env_map: &HashMap<String, String>,
) -> AllowDenyPaths {
    let SandboxPolicy::WorkspaceWrite {
        writable_roots,
        exclude_tmpdir_env_var,
        ..
    } = policy
    else {
        return AllowDenyPaths::default();
    };

    compute_workspace_write_allow_paths(WorkspaceWriteAllowPathInputs {
        policy_cwd,
        command_cwd,
        env_map,
        writable_roots,
        include_tmp_env_vars: !exclude_tmpdir_env_var,
    })
}

struct WorkspaceWriteAllowPathInputs<'a> {
    policy_cwd: &'a Path,
    command_cwd: &'a Path,
    env_map: &'a HashMap<String, String>,
    writable_roots: &'a [AbsolutePathBuf],
    include_tmp_env_vars: bool,
}

fn compute_workspace_write_allow_paths(input: WorkspaceWriteAllowPathInputs<'_>) -> AllowDenyPaths {
    let mut allow: HashSet<PathBuf> = HashSet::new();
    let mut deny: HashSet<PathBuf> = HashSet::new();

    let mut add_allow_path = |p: PathBuf| {
        if p.exists() {
            allow.insert(p);
        }
    };
    let mut add_deny_path = |p: PathBuf| {
        if p.exists() {
            deny.insert(p);
        }
    };

    let add_writable_root =
        |root: PathBuf,
         policy_cwd: &Path,
         add_allow: &mut dyn FnMut(PathBuf),
         add_deny: &mut dyn FnMut(PathBuf)| {
            let candidate = if root.is_absolute() {
                root
            } else {
                policy_cwd.join(root)
            };
            let canonical = canonicalize(&candidate).unwrap_or(candidate);
            add_allow(canonical.clone());

            for protected_subdir in [".git", ".codex", ".agents"] {
                let protected_entry = canonical.join(protected_subdir);
                if protected_entry.exists() {
                    add_deny(protected_entry);
                }
            }
        };

    add_writable_root(
        input.command_cwd.to_path_buf(),
        input.policy_cwd,
        &mut add_allow_path,
        &mut add_deny_path,
    );

    for root in input.writable_roots {
        add_writable_root(
            root.as_path().to_path_buf(),
            input.policy_cwd,
            &mut add_allow_path,
            &mut add_deny_path,
        );
    }
    if input.include_tmp_env_vars {
        for key in ["TEMP", "TMP"] {
            if let Some(v) = input.env_map.get(key) {
                let abs = PathBuf::from(v);
                add_allow_path(abs);
            } else if let Ok(v) = std::env::var(key) {
                let abs = PathBuf::from(v);
                add_allow_path(abs);
            }
        }
    }
    AllowDenyPaths { allow, deny }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn workspace_paths<'a>(
        policy_cwd: &'a Path,
        command_cwd: &'a Path,
        env_map: &'a HashMap<String, String>,
        writable_roots: &'a [AbsolutePathBuf],
        include_tmp_env_vars: bool,
    ) -> AllowDenyPaths {
        compute_workspace_write_allow_paths(WorkspaceWriteAllowPathInputs {
            policy_cwd,
            command_cwd,
            env_map,
            writable_roots,
            include_tmp_env_vars,
        })
    }

    #[test]
    fn includes_additional_writable_roots() {
        let tmp = TempDir::new().expect("tempdir");
        let command_cwd = tmp.path().join("workspace");
        let extra_root = tmp.path().join("extra");
        let _ = fs::create_dir_all(&command_cwd);
        let _ = fs::create_dir_all(&extra_root);

        let writable_roots = vec![AbsolutePathBuf::try_from(extra_root.as_path()).unwrap()];
        let paths = workspace_paths(
            &command_cwd,
            &command_cwd,
            &HashMap::new(),
            &writable_roots,
            /*include_tmp_env_vars*/ true,
        );

        assert!(paths
            .allow
            .contains(&dunce::canonicalize(&command_cwd).unwrap()));
        assert!(paths
            .allow
            .contains(&dunce::canonicalize(&extra_root).unwrap()));
        assert!(paths.deny.is_empty(), "no deny paths expected");
    }

    #[test]
    fn excludes_tmp_env_vars_when_requested() {
        let tmp = TempDir::new().expect("tempdir");
        let command_cwd = tmp.path().join("workspace");
        let temp_dir = tmp.path().join("temp");
        let _ = fs::create_dir_all(&command_cwd);
        let _ = fs::create_dir_all(&temp_dir);

        let mut env_map = HashMap::new();
        env_map.insert("TEMP".into(), temp_dir.to_string_lossy().to_string());

        let paths = workspace_paths(
            &command_cwd,
            &command_cwd,
            &env_map,
            &[],
            /*include_tmp_env_vars*/ false,
        );

        assert!(paths
            .allow
            .contains(&dunce::canonicalize(&command_cwd).unwrap()));
        assert!(!paths
            .allow
            .contains(&dunce::canonicalize(&temp_dir).unwrap()));
        assert!(paths.deny.is_empty(), "no deny paths expected");
    }

    #[test]
    fn denies_git_dir_inside_writable_root() {
        let tmp = TempDir::new().expect("tempdir");
        let command_cwd = tmp.path().join("workspace");
        let git_dir = command_cwd.join(".git");
        let _ = fs::create_dir_all(&git_dir);

        let paths = workspace_paths(
            &command_cwd,
            &command_cwd,
            &HashMap::new(),
            &[],
            /*include_tmp_env_vars*/ false,
        );
        let expected_allow: HashSet<PathBuf> = [dunce::canonicalize(&command_cwd).unwrap()]
            .into_iter()
            .collect();
        let expected_deny: HashSet<PathBuf> = [dunce::canonicalize(&git_dir).unwrap()]
            .into_iter()
            .collect();

        assert_eq!(expected_allow, paths.allow);
        assert_eq!(expected_deny, paths.deny);
    }

    #[test]
    fn denies_git_file_inside_writable_root() {
        let tmp = TempDir::new().expect("tempdir");
        let command_cwd = tmp.path().join("workspace");
        let git_file = command_cwd.join(".git");
        let _ = fs::create_dir_all(&command_cwd);
        let _ = fs::write(&git_file, "gitdir: .git/worktrees/example");

        let paths = workspace_paths(
            &command_cwd,
            &command_cwd,
            &HashMap::new(),
            &[],
            /*include_tmp_env_vars*/ false,
        );
        let expected_allow: HashSet<PathBuf> = [dunce::canonicalize(&command_cwd).unwrap()]
            .into_iter()
            .collect();
        let expected_deny: HashSet<PathBuf> = [dunce::canonicalize(&git_file).unwrap()]
            .into_iter()
            .collect();

        assert_eq!(expected_allow, paths.allow);
        assert_eq!(expected_deny, paths.deny);
    }

    #[test]
    fn denies_codex_and_agents_inside_writable_root() {
        let tmp = TempDir::new().expect("tempdir");
        let command_cwd = tmp.path().join("workspace");
        let codex_dir = command_cwd.join(".codex");
        let agents_dir = command_cwd.join(".agents");
        let _ = fs::create_dir_all(&codex_dir);
        let _ = fs::create_dir_all(&agents_dir);

        let paths = workspace_paths(
            &command_cwd,
            &command_cwd,
            &HashMap::new(),
            &[],
            /*include_tmp_env_vars*/ false,
        );
        let expected_allow: HashSet<PathBuf> = [dunce::canonicalize(&command_cwd).unwrap()]
            .into_iter()
            .collect();
        let expected_deny: HashSet<PathBuf> = [
            dunce::canonicalize(&codex_dir).unwrap(),
            dunce::canonicalize(&agents_dir).unwrap(),
        ]
        .into_iter()
        .collect();

        assert_eq!(expected_allow, paths.allow);
        assert_eq!(expected_deny, paths.deny);
    }

    #[test]
    fn skips_protected_subdirs_when_missing() {
        let tmp = TempDir::new().expect("tempdir");
        let command_cwd = tmp.path().join("workspace");
        let _ = fs::create_dir_all(&command_cwd);

        let paths = workspace_paths(
            &command_cwd,
            &command_cwd,
            &HashMap::new(),
            &[],
            /*include_tmp_env_vars*/ false,
        );
        assert_eq!(paths.allow.len(), 1);
        assert!(paths.deny.is_empty(), "no deny when protected dirs are absent");
    }
}
