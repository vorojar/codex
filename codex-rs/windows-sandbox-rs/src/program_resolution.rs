use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::path::PathBuf;

pub(crate) struct ResolvedSpawnCommand {
    pub(crate) argv: Vec<String>,
    pub(crate) application_name: Option<String>,
}

pub(crate) fn resolve_spawn_command(
    argv: &[String],
    cwd: &Path,
    env_map: &HashMap<String, String>,
) -> ResolvedSpawnCommand {
    let Some(program) = argv.first() else {
        return ResolvedSpawnCommand {
            argv: Vec::new(),
            application_name: None,
        };
    };

    let Some(resolved_program) = resolve_program_path(program, cwd, env_map) else {
        return ResolvedSpawnCommand {
            argv: argv.to_vec(),
            application_name: None,
        };
    };

    let resolved_str = resolved_program.to_string_lossy().to_string();
    let mut resolved_argv = argv.to_vec();
    resolved_argv[0] = resolved_str.clone();
    ResolvedSpawnCommand {
        argv: resolved_argv,
        application_name: Some(resolved_str),
    }
}

fn resolve_program_path(
    program: &str,
    cwd: &Path,
    env_map: &HashMap<String, String>,
) -> Option<PathBuf> {
    let program_path = Path::new(program);
    if has_path_qualifier(program_path) {
        return resolve_explicit_program_path(program_path, cwd, env_map);
    }

    if is_cmd_program(program) {
        let comspec = env_map
            .get("ComSpec")
            .cloned()
            .or_else(|| env::var("ComSpec").ok());
        if let Some(comspec) = comspec {
            let comspec_path = PathBuf::from(comspec);
            if comspec_path.is_file() && !is_windowsapps_path(&comspec_path) {
                return Some(comspec_path);
            }
        }
    }

    search_path_for_program(program_path, env_map)
}

fn resolve_explicit_program_path(
    program_path: &Path,
    cwd: &Path,
    env_map: &HashMap<String, String>,
) -> Option<PathBuf> {
    let candidate = if program_path.is_absolute() {
        program_path.to_path_buf()
    } else {
        cwd.join(program_path)
    };
    if candidate.is_file() && !is_windowsapps_path(&candidate) {
        return Some(candidate);
    }
    if is_windowsapps_path(&candidate) {
        let file_name = program_path.file_name()?.to_string_lossy().to_string();
        return resolve_program_path(&file_name, cwd, env_map);
    }
    None
}

fn search_path_for_program(
    program_path: &Path,
    env_map: &HashMap<String, String>,
) -> Option<PathBuf> {
    let search_path = env_map
        .get("PATH")
        .cloned()
        .or_else(|| env::var("PATH").ok())?;
    let path_exts = path_extensions(env_map);
    let has_extension = program_path.extension().is_some();

    for dir in env::split_paths(&search_path) {
        let direct_candidate = dir.join(program_path);
        if direct_candidate.is_file() && !is_windowsapps_path(&direct_candidate) {
            return Some(direct_candidate);
        }

        if has_extension {
            continue;
        }

        for ext in &path_exts {
            let candidate = dir.join(program_path).with_extension(ext);
            if candidate.is_file() && !is_windowsapps_path(&candidate) {
                return Some(candidate);
            }
        }
    }

    None
}

fn path_extensions(env_map: &HashMap<String, String>) -> Vec<String> {
    env_map
        .get("PATHEXT")
        .cloned()
        .or_else(|| env::var("PATHEXT").ok())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string())
        .split(';')
        .filter(|ext| !ext.is_empty())
        .map(|ext| ext.trim_start_matches('.').to_string())
        .collect()
}

fn has_path_qualifier(path: &Path) -> bool {
    path.is_absolute() || path.parent().is_some()
}

fn is_cmd_program(program: &str) -> bool {
    program.eq_ignore_ascii_case("cmd") || program.eq_ignore_ascii_case("cmd.exe")
}

fn is_windowsapps_path(path: &Path) -> bool {
    let normalized = path
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    normalized.contains("\\program files\\windowsapps\\")
        || normalized.contains("\\appdata\\local\\microsoft\\windowsapps\\")
}

#[cfg(test)]
mod tests {
    use super::is_windowsapps_path;
    use super::resolve_spawn_command;
    use pretty_assertions::assert_eq;
    use std::collections::HashMap;
    use std::env;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn windowsapps_path_detection_handles_forward_slashes() {
        assert!(is_windowsapps_path(std::path::Path::new(
            "C:/Users/alice/AppData/Local/Microsoft/WindowsApps/pwsh.exe"
        )));
        assert!(is_windowsapps_path(std::path::Path::new(
            "C:/Program Files/WindowsApps/Foo/tool.exe"
        )));
        assert!(!is_windowsapps_path(std::path::Path::new(
            "C:/Windows/System32/cmd.exe"
        )));
    }

    #[test]
    fn resolve_spawn_command_skips_windowsapps_aliases() {
        let temp = TempDir::new().expect("tempdir");
        let alias_dir = temp.path().join("AppData/Local/Microsoft/WindowsApps");
        let real_dir = temp.path().join("real-bin");
        fs::create_dir_all(&alias_dir).expect("create alias dir");
        fs::create_dir_all(&real_dir).expect("create real dir");
        fs::write(alias_dir.join("tool.cmd"), "@echo off\r\n").expect("write alias");
        fs::write(real_dir.join("tool.cmd"), "@echo off\r\n").expect("write real");

        let mut env_map = HashMap::new();
        let path = env::join_paths([alias_dir.as_path(), real_dir.as_path()])
            .expect("join path")
            .to_string_lossy()
            .to_string();
        env_map.insert("PATH".to_string(), path);
        env_map.insert("PATHEXT".to_string(), ".EXE;.CMD".to_string());

        let resolved = resolve_spawn_command(
            &["tool".to_string(), "/c".to_string()],
            temp.path(),
            &env_map,
        );
        assert_eq!(
            resolved.argv[0],
            real_dir.join("tool.cmd").to_string_lossy().to_string()
        );
        assert_eq!(resolved.application_name, Some(resolved.argv[0].clone()));
    }

    #[test]
    fn resolve_spawn_command_uses_comspec_for_cmd() {
        let temp = TempDir::new().expect("tempdir");
        let system32 = temp.path().join("Windows/System32");
        fs::create_dir_all(&system32).expect("create system32");
        let cmd = system32.join("cmd.exe");
        fs::write(&cmd, "stub").expect("write cmd");

        let mut env_map = HashMap::new();
        env_map.insert("ComSpec".to_string(), cmd.to_string_lossy().to_string());

        let resolved = resolve_spawn_command(
            &["cmd".to_string(), "/c".to_string()],
            temp.path(),
            &env_map,
        );
        assert_eq!(resolved.argv[0], cmd.to_string_lossy().to_string());
        assert_eq!(resolved.application_name, Some(resolved.argv[0].clone()));
    }
}
