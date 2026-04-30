use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::NetworkSandboxPolicy;
use std::path::Path;

pub fn summarize_permission_profile(permission_profile: &PermissionProfile, cwd: &Path) -> String {
    match permission_profile {
        PermissionProfile::Disabled => "danger-full-access".to_string(),
        PermissionProfile::External { network } => {
            summary_with_network("external-sandbox", network.is_enabled())
        }
        PermissionProfile::Managed {
            file_system,
            network,
        } => summarize_managed_profile(&file_system.to_sandbox_policy(), *network, cwd),
    }
}

fn summarize_managed_profile(
    file_system: &FileSystemSandboxPolicy,
    network: NetworkSandboxPolicy,
    cwd: &Path,
) -> String {
    let network_enabled = network.is_enabled();
    if file_system.has_full_disk_write_access() {
        if network_enabled {
            return "danger-full-access".to_string();
        }
        return custom_summary(network_enabled);
    }

    let writable_roots = file_system.get_writable_roots_with_cwd(cwd);
    if writable_roots.is_empty() {
        if file_system.has_full_disk_read_access() {
            return summary_with_network("read-only", network_enabled);
        }
        return custom_summary(network_enabled);
    }

    let writable_entries = writable_roots
        .iter()
        .map(|root| writable_root_display(root.root.as_path(), cwd))
        .collect::<Vec<_>>();
    summary_with_network(
        &format!("workspace-write [{}]", writable_entries.join(", ")),
        network_enabled,
    )
}

fn writable_root_display(root: &Path, cwd: &Path) -> String {
    if root == cwd {
        return "workdir".to_string();
    }
    if cfg!(unix) && root == Path::new("/tmp") {
        return "/tmp".to_string();
    }
    if root == std::env::temp_dir() {
        return "$TMPDIR".to_string();
    }
    root.display().to_string()
}

fn summary_with_network(base: &str, network_enabled: bool) -> String {
    if network_enabled {
        format!("{base} (network access enabled)")
    } else {
        base.to_string()
    }
}

fn custom_summary(network_enabled: bool) -> String {
    summary_with_network("custom permissions", network_enabled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::permissions::FileSystemAccessMode;
    use codex_protocol::permissions::FileSystemPath;
    use codex_protocol::permissions::FileSystemSandboxEntry;
    use codex_protocol::permissions::FileSystemSpecialPath;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use pretty_assertions::assert_eq;

    #[test]
    fn summarizes_external_sandbox_without_network_access_suffix() {
        let summary = summarize_permission_profile(
            &PermissionProfile::External {
                network: NetworkSandboxPolicy::Restricted,
            },
            Path::new("/repo"),
        );
        assert_eq!(summary, "external-sandbox");
    }

    #[test]
    fn summarizes_external_sandbox_with_enabled_network() {
        let summary = summarize_permission_profile(
            &PermissionProfile::External {
                network: NetworkSandboxPolicy::Enabled,
            },
            Path::new("/repo"),
        );
        assert_eq!(summary, "external-sandbox (network access enabled)");
    }

    #[test]
    fn summarizes_read_only_with_enabled_network() {
        let file_system = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Read,
        }]);
        let profile = PermissionProfile::from_runtime_permissions(
            &file_system,
            NetworkSandboxPolicy::Enabled,
        );
        let summary = summarize_permission_profile(&profile, Path::new("/repo"));
        assert_eq!(summary, "read-only (network access enabled)");
    }

    #[test]
    fn unrestricted_filesystem_without_network_is_custom_permissions() {
        let profile = PermissionProfile::from_runtime_permissions(
            &FileSystemSandboxPolicy::unrestricted(),
            NetworkSandboxPolicy::Restricted,
        );
        let summary = summarize_permission_profile(&profile, Path::new("/repo"));
        assert_eq!(summary, "custom permissions");
    }

    #[test]
    fn workspace_write_summary_still_includes_network_access() {
        let root = if cfg!(windows) { "C:\\repo" } else { "/repo" };
        let writable_root = AbsolutePathBuf::try_from(root).unwrap();
        let cwd = if cfg!(windows) {
            "C:\\workdir"
        } else {
            "/workdir"
        };
        let profile = PermissionProfile::workspace_write_with(
            std::slice::from_ref(&writable_root),
            NetworkSandboxPolicy::Enabled,
            /*exclude_tmpdir_env_var*/ true,
            /*exclude_slash_tmp*/ true,
        );
        let summary = summarize_permission_profile(&profile, Path::new(cwd));
        assert_eq!(
            summary,
            format!(
                "workspace-write [workdir, {}] (network access enabled)",
                writable_root.to_string_lossy()
            )
        );
    }
}
