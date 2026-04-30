use std::path::PathBuf;

use codex_utils_absolute_path::AbsolutePathBuf;

use crate::command_safety::try_parse_powershell_ast_commands;
use crate::shell_detect::ShellType;
use crate::shell_detect::detect_shell_type;

const POWERSHELL_FLAGS: &[&str] = &["-nologo", "-noprofile", "-command", "-c"];
const POWERSHELL_NO_ARG_PARSE_FLAGS: &[&str] =
    &["-nologo", "-noprofile", "-noninteractive", "-mta", "-sta"];
const POWERSHELL_VALUE_PARSE_FLAGS: &[&str] =
    &["-windowstyle", "-executionpolicy", "-workingdirectory"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PowershellCommandSequenceParseMode {
    ExecPolicy,
    SafeCommand,
}

/// Prefixed command for powershell shell calls to force UTF-8 console output.
pub const UTF8_OUTPUT_PREFIX: &str = "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8;\n";

pub fn prefix_powershell_script_with_utf8(command: &[String]) -> Vec<String> {
    let Some((_, script)) = extract_powershell_command(command) else {
        return command.to_vec();
    };

    let trimmed = script.trim_start();
    let script = if trimmed.starts_with(UTF8_OUTPUT_PREFIX) {
        script.to_string()
    } else {
        format!("{UTF8_OUTPUT_PREFIX}{script}")
    };

    let mut command: Vec<String> = command[..(command.len() - 1)]
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    command.push(script);
    command
}

/// Extract the PowerShell script body from an invocation such as:
///
/// - ["pwsh", "-NoProfile", "-Command", "Get-ChildItem -Recurse | Select-String foo"]
/// - ["powershell.exe", "-Command", "Write-Host hi"]
/// - ["powershell", "-NoLogo", "-NoProfile", "-Command", "...script..."]
///
/// Returns (`shell`, `script`) when the first arg is a PowerShell executable and a
/// `-Command` (or `-c`) flag is present followed by a script string.
pub fn extract_powershell_command(command: &[String]) -> Option<(&str, &str)> {
    if command.len() < 3 {
        return None;
    }

    let shell = &command[0];
    if !matches!(
        detect_shell_type(&PathBuf::from(shell)),
        Some(ShellType::PowerShell)
    ) {
        return None;
    }

    // Find the first occurrence of -Command (accept common short alias -c as well)
    let mut i = 1usize;
    while i + 1 < command.len() {
        let flag = &command[i];
        // Reject unknown flags
        if !POWERSHELL_FLAGS.contains(&flag.to_ascii_lowercase().as_str()) {
            return None;
        }
        if flag.eq_ignore_ascii_case("-Command") || flag.eq_ignore_ascii_case("-c") {
            let script = &command[i + 1];
            return Some((shell, script));
        }
        i += 1;
    }
    None
}

/// Recover discrete inner command vectors from an explicit one-layer PowerShell wrapper.
///
/// This recognizes top-level `powershell` / `powershell.exe` / `pwsh` / `pwsh.exe`
/// invocations with an explicit `-Command` / `/Command` / `-c` body and parses that
/// script with the PowerShell AST parser. Unsupported or opaque forms such as
/// `-EncodedCommand` return `None`.
pub fn try_parse_powershell_command_sequence(
    command: &[String],
    mode: PowershellCommandSequenceParseMode,
) -> Option<Vec<Vec<String>>> {
    let (executable, args) = command.split_first()?;
    if is_powershell_executable(executable) {
        parse_powershell_invocation(executable, args, mode)
    } else {
        None
    }
}

fn parse_powershell_invocation(
    executable: &str,
    args: &[String],
    mode: PowershellCommandSequenceParseMode,
) -> Option<Vec<Vec<String>>> {
    if args.is_empty() {
        return None;
    }

    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        let lower = arg.to_ascii_lowercase();
        match lower.as_str() {
            "-command" | "/command" | "-c" => {
                let script = args.get(idx + 1)?;
                if idx + 2 != args.len() {
                    return None;
                }
                return parse_powershell_script_to_commands(executable, script);
            }
            _ if lower.starts_with("-command:") || lower.starts_with("/command:") => {
                if idx + 1 != args.len() {
                    return None;
                }
                let script = arg.split_once(':')?.1;
                return parse_powershell_script_to_commands(executable, script);
            }
            _ if is_powershell_no_arg_parse_flag(&lower) => {
                idx += 1;
                continue;
            }
            _ if is_powershell_value_parse_flag(&lower) => {
                if mode == PowershellCommandSequenceParseMode::SafeCommand {
                    return None;
                }
                args.get(idx + 1)?;
                idx += 2;
                continue;
            }
            _ if is_powershell_value_parse_flag_with_inline_value(&lower) => {
                if mode == PowershellCommandSequenceParseMode::SafeCommand {
                    return None;
                }
                idx += 1;
                continue;
            }
            _ if is_unsupported_powershell_parse_flag(&lower)
                || has_unsupported_powershell_parse_flag_inline_value(&lower) =>
            {
                return None;
            }
            _ if looks_like_powershell_flag(&lower) => {
                if mode == PowershellCommandSequenceParseMode::SafeCommand {
                    return None;
                }

                idx += powershell_wrapper_flag_length(args, idx);
                continue;
            }
            _ => {
                if mode == PowershellCommandSequenceParseMode::ExecPolicy {
                    return None;
                }

                let script = join_arguments_as_script(&args[idx..]);
                return parse_powershell_script_to_commands(executable, &script);
            }
        }
    }

    None
}

pub(crate) fn parse_powershell_script_to_commands(
    executable: &str,
    script: &str,
) -> Option<Vec<Vec<String>>> {
    try_parse_powershell_ast_commands(executable, script)
}

pub(crate) fn is_powershell_executable(exe: &str) -> bool {
    let executable_name = std::path::Path::new(exe)
        .file_name()
        .and_then(|osstr| osstr.to_str())
        .unwrap_or(exe)
        .to_ascii_lowercase();

    matches!(
        executable_name.as_str(),
        "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe"
    )
}

pub(crate) fn join_arguments_as_script(args: &[String]) -> String {
    let mut words = Vec::with_capacity(args.len());
    if let Some((first, rest)) = args.split_first() {
        words.push(first.clone());
        for arg in rest {
            words.push(quote_argument(arg));
        }
    }
    words.join(" ")
}

fn quote_argument(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }

    if arg.chars().all(|ch| !ch.is_whitespace()) {
        return arg.to_string();
    }

    format!("'{}'", arg.replace('\'', "''"))
}

fn looks_like_powershell_flag(lower: &str) -> bool {
    lower.starts_with('-') || lower.starts_with('/')
}

fn is_powershell_no_arg_parse_flag(lower: &str) -> bool {
    POWERSHELL_NO_ARG_PARSE_FLAGS.contains(&lower)
}

fn is_powershell_value_parse_flag(lower: &str) -> bool {
    POWERSHELL_VALUE_PARSE_FLAGS.contains(&lower)
        || matches!(
            lower,
            "/windowstyle" | "/executionpolicy" | "/workingdirectory"
        )
}

fn is_powershell_value_parse_flag_with_inline_value(lower: &str) -> bool {
    matches!(
        split_flag_inline_value(lower),
        Some((
            "-windowstyle"
                | "/windowstyle"
                | "-executionpolicy"
                | "/executionpolicy"
                | "-workingdirectory"
                | "/workingdirectory",
            _
        ))
    )
}

fn is_unsupported_powershell_parse_flag(lower: &str) -> bool {
    matches!(lower, "-encodedcommand" | "-ec" | "-file" | "/file")
}

fn has_unsupported_powershell_parse_flag_inline_value(lower: &str) -> bool {
    matches!(
        split_flag_inline_value(lower),
        Some(("-encodedcommand" | "-ec" | "-file" | "/file", _))
    )
}

fn split_flag_inline_value(lower: &str) -> Option<(&str, &str)> {
    lower.split_once(':')
}

fn powershell_wrapper_flag_length(args: &[String], idx: usize) -> usize {
    let Some(next_arg) = args.get(idx + 1) else {
        return 1;
    };

    if looks_like_powershell_flag(&next_arg.to_ascii_lowercase()) {
        1
    } else {
        2
    }
}

/// This function attempts to find a powershell.exe executable on the system.
pub fn try_find_powershell_executable_blocking() -> Option<AbsolutePathBuf> {
    try_find_powershellish_executable_in_path(&["powershell.exe"])
}

/// This function attempts to find a pwsh.exe executable on the system.
/// Note that pwsh.exe and powershell.exe are different executables:
///
/// - pwsh.exe is the cross-platform PowerShell Core (v6+) executable
/// - powershell.exe is the Windows PowerShell (v5.1 and earlier) executable
///
/// Further, while powershell.exe is included by default on Windows systems,
/// pwsh.exe must be installed separately by the user. And even when the user
/// has installed pwsh.exe, it may not be available in the system PATH, in which
/// case we attempt to locate it via other means.
pub fn try_find_pwsh_executable_blocking() -> Option<AbsolutePathBuf> {
    if let Some(ps_home) = std::process::Command::new("cmd")
        .args(["/C", "pwsh", "-NoProfile", "-Command", "$PSHOME"])
        .output()
        .ok()
        .and_then(|out| {
            if !out.status.success() {
                return None;
            }
            let stdout = String::from_utf8_lossy(&out.stdout);
            let trimmed = stdout.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
    {
        let candidate = AbsolutePathBuf::resolve_path_against_base("pwsh.exe", &ps_home);

        if is_powershellish_executable_available(candidate.as_path()) {
            return Some(candidate);
        }
    }

    try_find_powershellish_executable_in_path(&["pwsh.exe"])
}

fn try_find_powershellish_executable_in_path(candidates: &[&str]) -> Option<AbsolutePathBuf> {
    for candidate in candidates {
        let Ok(resolved_path) = which::which(candidate) else {
            continue;
        };

        if !is_powershellish_executable_available(&resolved_path) {
            continue;
        }

        let Ok(abs_path) = AbsolutePathBuf::from_absolute_path(resolved_path) else {
            continue;
        };

        return Some(abs_path);
    }

    None
}

fn is_powershellish_executable_available(powershell_or_pwsh_exe: &std::path::Path) -> bool {
    // This test works for both powershell.exe and pwsh.exe.
    std::process::Command::new(powershell_or_pwsh_exe)
        .args(["-NoLogo", "-NoProfile", "-Command", "Write-Output ok"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::PowershellCommandSequenceParseMode;
    use super::extract_powershell_command;
    use super::try_parse_powershell_command_sequence;
    use pretty_assertions::assert_eq;

    #[test]
    fn extracts_basic_powershell_command() {
        let cmd = vec![
            "powershell".to_string(),
            "-Command".to_string(),
            "Write-Host hi".to_string(),
        ];
        let (_shell, script) = extract_powershell_command(&cmd).expect("extract");
        assert_eq!(script, "Write-Host hi");
    }

    #[test]
    fn extracts_lowercase_flags() {
        let cmd = vec![
            "powershell".to_string(),
            "-nologo".to_string(),
            "-command".to_string(),
            "Write-Host hi".to_string(),
        ];
        let (_shell, script) = extract_powershell_command(&cmd).expect("extract");
        assert_eq!(script, "Write-Host hi");
    }

    #[test]
    fn extracts_full_path_powershell_command() {
        let command = if cfg!(windows) {
            "C:\\windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe".to_string()
        } else {
            "/usr/local/bin/powershell.exe".to_string()
        };
        let cmd = vec![command, "-Command".to_string(), "Write-Host hi".to_string()];
        let (_shell, script) = extract_powershell_command(&cmd).expect("extract");
        assert_eq!(script, "Write-Host hi");
    }

    #[test]
    fn extracts_with_noprofile_and_alias() {
        let cmd = vec![
            "pwsh".to_string(),
            "-NoProfile".to_string(),
            "-c".to_string(),
            "Get-ChildItem | Select-String foo".to_string(),
        ];
        let (_shell, script) = extract_powershell_command(&cmd).expect("extract");
        assert_eq!(script, "Get-ChildItem | Select-String foo");
    }

    #[cfg(windows)]
    #[test]
    fn exec_policy_parsing_ignores_ordinary_wrapper_flags() {
        let command = vec![
            "powershell.exe".to_string(),
            "-Version".to_string(),
            "5.1".to_string(),
            "-NoExit".to_string(),
            "-Command".to_string(),
            "Get-Content 'foo bar'".to_string(),
        ];

        assert_eq!(
            try_parse_powershell_command_sequence(
                &command,
                PowershellCommandSequenceParseMode::ExecPolicy,
            ),
            Some(vec![
                vec!["Get-Content".to_string(), "foo bar".to_string(),]
            ]),
        );
    }
}
