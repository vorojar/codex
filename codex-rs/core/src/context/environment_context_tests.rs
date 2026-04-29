use crate::shell::ShellType;

use super::*;
use codex_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::protocol::TurnEnvironmentSelection;
use codex_utils_absolute_path::AbsolutePathBuf;
use core_test_support::test_path_buf;
use pretty_assertions::assert_eq;
use std::path::PathBuf;

fn fake_shell_name() -> String {
    let shell = crate::shell::Shell {
        shell_type: ShellType::Bash,
        shell_path: PathBuf::from("/bin/bash"),
        shell_snapshot: crate::shell::empty_shell_snapshot_receiver(),
    };
    shell.name().to_string()
}

fn environment(environment_id: &str, cwd: &str) -> TurnEnvironmentSelection {
    TurnEnvironmentSelection {
        environment_id: environment_id.to_string(),
        cwd: AbsolutePathBuf::try_from(PathBuf::from(cwd)).expect("absolute cwd"),
    }
}

fn turn_context_item(environments: Option<Vec<TurnEnvironmentSelection>>) -> TurnContextItem {
    TurnContextItem {
        turn_id: None,
        trace_id: None,
        cwd: test_path_buf("/repo"),
        environments,
        current_date: Some("2026-02-26".to_string()),
        timezone: Some("America/Los_Angeles".to_string()),
        approval_policy: AskForApproval::Never,
        sandbox_policy: SandboxPolicy::DangerFullAccess,
        permission_profile: None,
        network: None,
        file_system_sandbox_policy: None,
        model: "gpt-5".to_string(),
        personality: None,
        collaboration_mode: None,
        realtime_active: None,
        effort: None,
        summary: ReasoningSummaryConfig::Auto,
        user_instructions: None,
        developer_instructions: None,
        final_output_json_schema: None,
        truncation_policy: None,
    }
}

#[test]
fn serialize_workspace_write_environment_context() {
    let cwd = test_path_buf("/repo");
    let context = EnvironmentContext::new(
        Some(cwd.clone()),
        fake_shell_name(),
        Some("2026-02-26".to_string()),
        Some("America/Los_Angeles".to_string()),
        /*network*/ None,
        /*subagents*/ None,
    );

    let expected = format!(
        r#"<environment_context>
  <cwd>{cwd}</cwd>
  <shell>bash</shell>
  <current_date>2026-02-26</current_date>
  <timezone>America/Los_Angeles</timezone>
</environment_context>"#,
        cwd = cwd.display(),
    );

    assert_eq!(context.render(), expected);
}

#[test]
fn serialize_environment_context_with_network() {
    let network = NetworkContext::new(
        vec!["api.example.com".to_string(), "*.openai.com".to_string()],
        vec!["blocked.example.com".to_string()],
    );
    let context = EnvironmentContext::new(
        Some(test_path_buf("/repo")),
        fake_shell_name(),
        Some("2026-02-26".to_string()),
        Some("America/Los_Angeles".to_string()),
        Some(network),
        /*subagents*/ None,
    );

    let expected = format!(
        r#"<environment_context>
  <cwd>{}</cwd>
  <shell>bash</shell>
  <current_date>2026-02-26</current_date>
  <timezone>America/Los_Angeles</timezone>
  <network enabled="true">
    <allowed>api.example.com</allowed>
    <allowed>*.openai.com</allowed>
    <denied>blocked.example.com</denied>
  </network>
</environment_context>"#,
        test_path_buf("/repo").display()
    );

    assert_eq!(context.render(), expected);
}

#[test]
fn serialize_environment_context_with_multiple_environments() {
    let context = EnvironmentContext::new(
        Some(test_path_buf("/repo")),
        fake_shell_name(),
        Some("2026-02-26".to_string()),
        Some("America/Los_Angeles".to_string()),
        /*network*/ None,
        /*subagents*/ None,
    )
    .with_environments(Some(vec![
        environment("local", "/repo"),
        environment("remote", "/workspace"),
    ]));

    let expected = r#"<environment_context>
  <cwd>/repo</cwd>
  <environments>
    <environment id="local" cwd="/repo" primary="true" />
    <environment id="remote" cwd="/workspace" />
  </environments>
  <shell>bash</shell>
  <current_date>2026-02-26</current_date>
  <timezone>America/Los_Angeles</timezone>
</environment_context>"#;

    assert_eq!(context.render(), expected);
}

#[test]
fn diff_environment_context_renders_empty_environment_list_when_cleared() {
    let before = turn_context_item(Some(vec![
        environment("local", "/repo"),
        environment("remote", "/workspace"),
    ]));
    let after = EnvironmentContext::new(
        Some(test_path_buf("/repo")),
        fake_shell_name(),
        before.current_date.clone(),
        before.timezone.clone(),
        /*network*/ None,
        /*subagents*/ None,
    );

    let diff = EnvironmentContext::diff_from_turn_context_item(&before, &after);

    let expected = r#"<environment_context>
  <environments>
  </environments>
  <shell>bash</shell>
  <current_date>2026-02-26</current_date>
  <timezone>America/Los_Angeles</timezone>
</environment_context>"#;

    assert_eq!(diff.render(), expected);
}

#[test]
fn serialize_read_only_environment_context() {
    let context = EnvironmentContext::new(
        /*cwd*/ None,
        fake_shell_name(),
        Some("2026-02-26".to_string()),
        Some("America/Los_Angeles".to_string()),
        /*network*/ None,
        /*subagents*/ None,
    );

    let expected = r#"<environment_context>
  <shell>bash</shell>
  <current_date>2026-02-26</current_date>
  <timezone>America/Los_Angeles</timezone>
</environment_context>"#;

    assert_eq!(context.render(), expected);
}

#[test]
fn equals_except_shell_compares_cwd() {
    let context1 = EnvironmentContext::new(
        Some(PathBuf::from("/repo")),
        fake_shell_name(),
        /*current_date*/ None,
        /*timezone*/ None,
        /*network*/ None,
        /*subagents*/ None,
    );
    let context2 = EnvironmentContext::new(
        Some(PathBuf::from("/repo")),
        fake_shell_name(),
        /*current_date*/ None,
        /*timezone*/ None,
        /*network*/ None,
        /*subagents*/ None,
    );
    assert!(context1.equals_except_shell(&context2));
}

#[test]
fn equals_except_shell_compares_cwd_differences() {
    let context1 = EnvironmentContext::new(
        Some(PathBuf::from("/repo1")),
        fake_shell_name(),
        /*current_date*/ None,
        /*timezone*/ None,
        /*network*/ None,
        /*subagents*/ None,
    );
    let context2 = EnvironmentContext::new(
        Some(PathBuf::from("/repo2")),
        fake_shell_name(),
        /*current_date*/ None,
        /*timezone*/ None,
        /*network*/ None,
        /*subagents*/ None,
    );

    assert!(!context1.equals_except_shell(&context2));
}

#[test]
fn equals_except_shell_ignores_shell() {
    let context1 = EnvironmentContext::new(
        Some(PathBuf::from("/repo")),
        "bash".to_string(),
        /*current_date*/ None,
        /*timezone*/ None,
        /*network*/ None,
        /*subagents*/ None,
    );
    let context2 = EnvironmentContext::new(
        Some(PathBuf::from("/repo")),
        "zsh".to_string(),
        /*current_date*/ None,
        /*timezone*/ None,
        /*network*/ None,
        /*subagents*/ None,
    );

    assert!(context1.equals_except_shell(&context2));
}

#[test]
fn serialize_environment_context_with_subagents() {
    let context = EnvironmentContext::new(
        Some(test_path_buf("/repo")),
        fake_shell_name(),
        Some("2026-02-26".to_string()),
        Some("America/Los_Angeles".to_string()),
        /*network*/ None,
        Some("- agent-1: atlas\n- agent-2".to_string()),
    );

    let expected = format!(
        r#"<environment_context>
  <cwd>{}</cwd>
  <shell>bash</shell>
  <current_date>2026-02-26</current_date>
  <timezone>America/Los_Angeles</timezone>
  <subagents>
    - agent-1: atlas
    - agent-2
  </subagents>
</environment_context>"#,
        test_path_buf("/repo").display()
    );

    assert_eq!(context.render(), expected);
}
