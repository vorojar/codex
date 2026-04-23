//! Rollout sibling session-state sidecars that track live session state.

use crate::INTERACTIVE_SESSION_SOURCES;
use anyhow::Context;
use anyhow::Result;
use chrono::SecondsFormat;
use chrono::Utc;
use codex_protocol::ThreadId;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_terminal_detection::TerminalAttachment;
use serde::Deserialize;
use serde::Serialize;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

const SESSION_STATE_SCHEMA_VERSION: u32 = 2;
const ROOT_TURN_LEASE_MINUTES: i64 = 15;
const SESSION_LEASE_SECONDS: i64 = 60;
pub const SESSION_HEARTBEAT_SECONDS: u64 = 15;

/// Persisted rollout sibling state for the current interactive session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionStateSidecar {
    /// Sidecar schema version.
    pub schema_version: u32,
    /// UTC timestamp for the latest refresh.
    pub updated_at: String,
    /// Current terminal attachment when Codex can identify one.
    pub terminal: Option<TerminalAttachment>,
    /// Whole-session lifecycle, independent of the current root turn.
    #[serde(default = "open_session_state")]
    pub session: SessionStateSession,
    /// Root turn lifecycle as observed by Codex.
    pub root_turn: SessionStateRootTurn,
    /// Unified-exec processes that remain alive beyond their startup response.
    pub background_exec: SessionStateBackgroundExec,
    /// Watchdog registrations that remain owned by this session.
    #[serde(default)]
    pub owner_watchdogs: SessionStateOwnerWatchdogs,
    /// Parent-edge metadata when this session is a spawned subagent.
    pub subagent: Option<SessionStateSubagent>,
}

/// Whole-session lifecycle for an interactive Codex session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SessionStateSession {
    Open { lease_expires_at: String },
    Closed,
}

/// Root user-turn lifecycle for the session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SessionStateRootTurn {
    Idle,
    Running {
        turn_id: String,
        started_at: String,
        updated_at: String,
        lease_expires_at: String,
    },
    Completed {
        turn_id: String,
        started_at: String,
        completed_at: String,
    },
    Aborted {
        turn_id: String,
        started_at: String,
        aborted_at: String,
    },
}

/// Background unified-exec processes currently owned by this session.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionStateBackgroundExec {
    pub processes: Vec<SessionStateBackgroundExecProcess>,
}

/// Live watchdog registrations currently owned by this session.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionStateOwnerWatchdogs {
    pub active_count: usize,
}

/// Persisted summary of a live unified-exec process.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionStateBackgroundExecProcess {
    pub process_id: String,
    pub call_id: String,
    pub command: String,
    pub cwd: PathBuf,
    pub started_at: String,
    pub updated_at: String,
    pub tty: bool,
}

/// Parent-edge metadata for a subagent session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionStateSubagent {
    pub parent_thread_id: ThreadId,
    pub depth: i32,
    pub edge_status: SessionStateSubagentEdgeStatus,
    pub agent_path: Option<String>,
    pub agent_nickname: Option<String>,
    pub agent_role: Option<String>,
}

/// Whether the parent still considers this subagent edge open.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStateSubagentEdgeStatus {
    Open,
    Closed,
}

/// Stateful writer for one rollout's session-state sidecar.
#[derive(Debug)]
pub struct SessionStateTracker {
    rollout_path: Option<PathBuf>,
    sidecar: Mutex<SessionStateSidecar>,
}

impl SessionStateTracker {
    /// Creates a tracker for an interactive rollout, or a disabled tracker otherwise.
    pub fn new(
        rollout_path: Option<PathBuf>,
        session_source: &SessionSource,
        terminal: Option<TerminalAttachment>,
    ) -> Self {
        let enabled_rollout_path =
            rollout_path.filter(|_| writes_session_state_sidecar_for_source(session_source));
        Self {
            rollout_path: enabled_rollout_path,
            sidecar: Mutex::new(build_idle_session_state_sidecar(
                terminal,
                subagent_metadata(session_source),
                Utc::now(),
            )),
        }
    }

    /// Creates a tracker that records state in memory but never writes a sidecar.
    pub fn disabled() -> Self {
        Self {
            rollout_path: None,
            sidecar: Mutex::new(build_idle_session_state_sidecar(
                /*terminal*/ None,
                /*subagent*/ None,
                Utc::now(),
            )),
        }
    }

    /// Writes the current snapshot when this tracker is enabled.
    pub fn write_current(&self) -> Result<bool> {
        let sidecar = self
            .sidecar
            .lock()
            .map_err(|_| anyhow::anyhow!("session state lock poisoned"))?
            .clone();
        self.write_snapshot(&sidecar)
    }

    /// Records that the root turn started and refreshes the live lease.
    pub fn note_root_turn_started(&self, turn_id: &str) -> Result<bool> {
        self.update(|current, now| {
            let timestamp = format_timestamp(now);
            refreshed_sidecar(
                current,
                now,
                SessionStateRootTurn::Running {
                    turn_id: turn_id.to_string(),
                    started_at: timestamp.clone(),
                    updated_at: timestamp,
                    lease_expires_at: format_timestamp(root_turn_lease_expires_at(now)),
                },
            )
        })
    }

    /// Refreshes the live lease for the current root turn.
    pub fn note_root_turn_observed(&self, turn_id: &str) -> Result<bool> {
        self.update(|current, now| {
            let root_turn = match &current.root_turn {
                SessionStateRootTurn::Running {
                    turn_id: current_turn_id,
                    started_at,
                    ..
                } if current_turn_id == turn_id => SessionStateRootTurn::Running {
                    turn_id: turn_id.to_string(),
                    started_at: started_at.clone(),
                    updated_at: format_timestamp(now),
                    lease_expires_at: format_timestamp(root_turn_lease_expires_at(now)),
                },
                _ => current.root_turn.clone(),
            };
            refreshed_sidecar(current, now, root_turn)
        })
    }

    /// Refreshes the whole-session lease even when no root turn is active.
    pub fn note_session_observed(&self) -> Result<bool> {
        self.update(|current, now| refreshed_sidecar(current, now, current.root_turn.clone()))
    }

    /// Records a terminal root turn.
    pub fn note_root_turn_completed(&self, turn_id: &str) -> Result<bool> {
        self.update(|current, now| {
            let timestamp = format_timestamp(now);
            let sidecar = refreshed_sidecar(
                current,
                now,
                SessionStateRootTurn::Completed {
                    turn_id: turn_id.to_string(),
                    started_at: root_turn_started_at(&current.root_turn, turn_id, &timestamp),
                    completed_at: timestamp,
                },
            );
            close_subagent_edge(sidecar)
        })
    }

    /// Records an aborted root turn.
    pub fn note_root_turn_aborted(&self, turn_id: &str) -> Result<bool> {
        self.update(|current, now| {
            let timestamp = format_timestamp(now);
            let sidecar = refreshed_sidecar(
                current,
                now,
                SessionStateRootTurn::Aborted {
                    turn_id: turn_id.to_string(),
                    started_at: root_turn_started_at(&current.root_turn, turn_id, &timestamp),
                    aborted_at: timestamp,
                },
            );
            close_subagent_edge(sidecar)
        })
    }

    /// Replaces the tracked background-process set.
    pub fn set_background_exec_processes(
        &self,
        processes: Vec<SessionStateBackgroundExecProcess>,
    ) -> Result<bool> {
        self.update(|current, now| SessionStateSidecar {
            schema_version: SESSION_STATE_SCHEMA_VERSION,
            updated_at: format_timestamp(now),
            session: refreshed_session_state(&current.session, now),
            background_exec: SessionStateBackgroundExec { processes },
            ..current.clone()
        })
    }

    /// Replaces the tracked owner-watchdog count.
    pub fn set_owner_watchdog_count(&self, active_count: usize) -> Result<bool> {
        self.update(|current, now| SessionStateSidecar {
            schema_version: SESSION_STATE_SCHEMA_VERSION,
            updated_at: format_timestamp(now),
            session: refreshed_session_state(&current.session, now),
            owner_watchdogs: SessionStateOwnerWatchdogs { active_count },
            ..current.clone()
        })
    }

    /// Records that the whole session has closed cleanly.
    pub fn note_session_closed(&self) -> Result<bool> {
        self.update(|current, now| SessionStateSidecar {
            schema_version: SESSION_STATE_SCHEMA_VERSION,
            updated_at: format_timestamp(now),
            session: SessionStateSession::Closed,
            ..current.clone()
        })
    }

    fn update(
        &self,
        build: impl FnOnce(&SessionStateSidecar, chrono::DateTime<Utc>) -> SessionStateSidecar,
    ) -> Result<bool> {
        self.update_at(Utc::now(), build)
    }

    fn update_at(
        &self,
        now: chrono::DateTime<Utc>,
        build: impl FnOnce(&SessionStateSidecar, chrono::DateTime<Utc>) -> SessionStateSidecar,
    ) -> Result<bool> {
        let sidecar = {
            let mut guard = self
                .sidecar
                .lock()
                .map_err(|_| anyhow::anyhow!("session state lock poisoned"))?;
            *guard = build(&guard, now);
            guard.clone()
        };
        self.write_snapshot(&sidecar)
    }

    fn write_snapshot(&self, sidecar: &SessionStateSidecar) -> Result<bool> {
        let Some(rollout_path) = &self.rollout_path else {
            return Ok(false);
        };
        write_session_state_sidecar(rollout_path, sidecar)?;
        Ok(true)
    }
}

/// Returns the sibling session-state sidecar path for a rollout path.
pub fn session_state_sidecar_path(rollout_path: &Path) -> PathBuf {
    rollout_path.with_extension("session-state.json")
}

/// Atomically overwrites the rollout sibling sidecar with the latest attachment state.
pub fn write_session_state_sidecar(
    rollout_path: &Path,
    sidecar: &SessionStateSidecar,
) -> Result<()> {
    let mut contents = serde_json::to_vec_pretty(&sidecar).context("serialize session state")?;
    contents.push(b'\n');
    write_file_atomically(
        session_state_sidecar_path(rollout_path).as_path(),
        &contents,
    )
}

/// Refreshes the session-state sidecar only for interactive sessions with a rollout path.
pub fn refresh_interactive_session_state_sidecar(
    rollout_path: Option<&Path>,
    session_source: &SessionSource,
    terminal: Option<&TerminalAttachment>,
) -> Result<bool> {
    let Some(rollout_path) = rollout_path else {
        return Ok(false);
    };

    if !INTERACTIVE_SESSION_SOURCES.contains(session_source) {
        return Ok(false);
    }

    let sidecar =
        build_idle_session_state_sidecar(terminal.cloned(), /*subagent*/ None, Utc::now());
    write_session_state_sidecar(rollout_path, &sidecar)?;
    Ok(true)
}

/// Moves an existing rollout sibling sidecar to track a renamed rollout file.
pub async fn move_session_state_sidecar_if_present(
    from_rollout_path: &Path,
    to_rollout_path: &Path,
) -> std::io::Result<()> {
    let from_sidecar = session_state_sidecar_path(from_rollout_path);
    let to_sidecar = session_state_sidecar_path(to_rollout_path);
    if let Some(parent) = to_sidecar.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    match tokio::fs::rename(&from_sidecar, &to_sidecar).await {
        Ok(()) => Ok(()),
        Err(initial_error) if initial_error.kind() == std::io::ErrorKind::NotFound => {
            match tokio::fs::metadata(&from_sidecar).await {
                Ok(_) => Err(initial_error),
                Err(metadata_error) if metadata_error.kind() == std::io::ErrorKind::NotFound => {
                    Ok(())
                }
                Err(metadata_error) => Err(metadata_error),
            }
        }
        Err(initial_error) => {
            #[cfg(target_os = "windows")]
            {
                if tokio::fs::try_exists(&to_sidecar).await.unwrap_or(false) {
                    tokio::fs::remove_file(&to_sidecar).await?;
                    tokio::fs::rename(&from_sidecar, &to_sidecar).await?;
                    return Ok(());
                }
            }

            Err(initial_error)
        }
    }
}

fn build_idle_session_state_sidecar(
    terminal: Option<TerminalAttachment>,
    subagent: Option<SessionStateSubagent>,
    updated_at: chrono::DateTime<Utc>,
) -> SessionStateSidecar {
    SessionStateSidecar {
        schema_version: SESSION_STATE_SCHEMA_VERSION,
        updated_at: updated_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        terminal,
        session: open_session_state_at(updated_at),
        root_turn: SessionStateRootTurn::Idle,
        background_exec: SessionStateBackgroundExec::default(),
        owner_watchdogs: SessionStateOwnerWatchdogs::default(),
        subagent,
    }
}

fn writes_session_state_sidecar_for_source(session_source: &SessionSource) -> bool {
    INTERACTIVE_SESSION_SOURCES.contains(session_source)
        || matches!(
            session_source,
            SessionSource::SubAgent(SubAgentSource::ThreadSpawn { .. })
        )
}

fn subagent_metadata(session_source: &SessionSource) -> Option<SessionStateSubagent> {
    match session_source {
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id,
            depth,
            agent_path,
            agent_nickname,
            agent_role,
        }) => Some(SessionStateSubagent {
            parent_thread_id: *parent_thread_id,
            depth: *depth,
            edge_status: SessionStateSubagentEdgeStatus::Open,
            agent_path: agent_path.as_ref().map(ToString::to_string),
            agent_nickname: agent_nickname.clone(),
            agent_role: agent_role.clone(),
        }),
        SessionSource::Cli
        | SessionSource::VSCode
        | SessionSource::Exec
        | SessionSource::Mcp
        | SessionSource::Internal(_)
        | SessionSource::Custom(_)
        | SessionSource::Internal(_)
        | SessionSource::SubAgent(_)
        | SessionSource::Unknown => None,
    }
}

fn close_subagent_edge(mut sidecar: SessionStateSidecar) -> SessionStateSidecar {
    if let Some(subagent) = &mut sidecar.subagent {
        subagent.edge_status = SessionStateSubagentEdgeStatus::Closed;
    }
    sidecar
}

fn refreshed_sidecar(
    current: &SessionStateSidecar,
    updated_at: chrono::DateTime<Utc>,
    root_turn: SessionStateRootTurn,
) -> SessionStateSidecar {
    SessionStateSidecar {
        schema_version: SESSION_STATE_SCHEMA_VERSION,
        updated_at: format_timestamp(updated_at),
        session: refreshed_session_state(&current.session, updated_at),
        root_turn,
        ..current.clone()
    }
}

fn root_turn_started_at(root_turn: &SessionStateRootTurn, turn_id: &str, fallback: &str) -> String {
    match root_turn {
        SessionStateRootTurn::Running {
            turn_id: current_turn_id,
            started_at,
            ..
        } if current_turn_id == turn_id => started_at.clone(),
        SessionStateRootTurn::Completed {
            turn_id: current_turn_id,
            started_at,
            ..
        }
        | SessionStateRootTurn::Aborted {
            turn_id: current_turn_id,
            started_at,
            ..
        } if current_turn_id == turn_id => started_at.clone(),
        SessionStateRootTurn::Idle
        | SessionStateRootTurn::Running { .. }
        | SessionStateRootTurn::Completed { .. }
        | SessionStateRootTurn::Aborted { .. } => fallback.to_string(),
    }
}

fn root_turn_lease_expires_at(updated_at: chrono::DateTime<Utc>) -> chrono::DateTime<Utc> {
    updated_at + chrono::Duration::minutes(ROOT_TURN_LEASE_MINUTES)
}

fn session_lease_expires_at(updated_at: chrono::DateTime<Utc>) -> chrono::DateTime<Utc> {
    updated_at + chrono::Duration::seconds(SESSION_LEASE_SECONDS)
}

fn open_session_state() -> SessionStateSession {
    open_session_state_at(Utc::now())
}

fn open_session_state_at(updated_at: chrono::DateTime<Utc>) -> SessionStateSession {
    SessionStateSession::Open {
        lease_expires_at: format_timestamp(session_lease_expires_at(updated_at)),
    }
}

fn refreshed_session_state(
    session: &SessionStateSession,
    updated_at: chrono::DateTime<Utc>,
) -> SessionStateSession {
    match session {
        SessionStateSession::Open { .. } => open_session_state_at(updated_at),
        SessionStateSession::Closed => SessionStateSession::Closed,
    }
}

fn format_timestamp(timestamp: chrono::DateTime<Utc>) -> String {
    timestamp.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn write_file_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = path.parent().with_context(|| {
        format!(
            "failed to compute parent directory for session state at {}",
            path.display()
        )
    })?;
    fs::create_dir_all(dir)
        .with_context(|| format!("failed to create session state dir {}", dir.display()))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .context("session state file name is not valid UTF-8")?;
    let tmp_path = dir.join(format!(".{file_name}.tmp-{}-{nonce}", std::process::id()));

    {
        let mut tmp_file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)
            .with_context(|| {
                format!(
                    "failed to create temp session state file at {}",
                    tmp_path.display()
                )
            })?;
        tmp_file.write_all(contents).with_context(|| {
            format!(
                "failed to write temp session state file at {}",
                tmp_path.display()
            )
        })?;
        tmp_file.sync_all().with_context(|| {
            format!(
                "failed to sync temp session state file at {}",
                tmp_path.display()
            )
        })?;
    }

    match fs::rename(&tmp_path, path) {
        Ok(()) => Ok(()),
        Err(initial_error) => {
            #[cfg(target_os = "windows")]
            {
                if path.exists() {
                    fs::remove_file(path).with_context(|| {
                        format!(
                            "failed to remove existing session state file at {} before replace",
                            path.display()
                        )
                    })?;
                    fs::rename(&tmp_path, path).with_context(|| {
                        format!(
                            "failed to replace session state file at {} with {}",
                            path.display(),
                            tmp_path.display()
                        )
                    })?;
                    return Ok(());
                }
            }

            let _ = fs::remove_file(&tmp_path);
            Err(initial_error).with_context(|| {
                format!(
                    "failed to replace session state file at {} with {}",
                    path.display(),
                    tmp_path.display()
                )
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::AgentPath;
    use codex_terminal_detection::TerminalAttachmentProvider;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    fn terminal_attachment(
        provider: TerminalAttachmentProvider,
        session_id: Option<&str>,
        tty: Option<&str>,
    ) -> TerminalAttachment {
        TerminalAttachment {
            provider,
            session_id: session_id.map(ToString::to_string),
            tty: tty.map(ToString::to_string),
        }
    }

    fn idle_sidecar(terminal: Option<TerminalAttachment>) -> SessionStateSidecar {
        build_idle_session_state_sidecar(
            terminal,
            /*subagent*/ None,
            chrono::DateTime::parse_from_rfc3339("2026-04-07T18:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        )
    }

    fn live_turn_sidecar(terminal: Option<TerminalAttachment>) -> SessionStateSidecar {
        SessionStateSidecar {
            schema_version: SESSION_STATE_SCHEMA_VERSION,
            updated_at: "2026-04-07T18:00:00Z".to_string(),
            terminal,
            session: SessionStateSession::Open {
                lease_expires_at: "2026-04-07T18:01:00Z".to_string(),
            },
            root_turn: SessionStateRootTurn::Running {
                turn_id: "turn-1".to_string(),
                started_at: "2026-04-07T17:58:00Z".to_string(),
                updated_at: "2026-04-07T18:00:00Z".to_string(),
                lease_expires_at: "2026-04-07T18:01:00Z".to_string(),
            },
            background_exec: SessionStateBackgroundExec {
                processes: vec![SessionStateBackgroundExecProcess {
                    process_id: "42".to_string(),
                    call_id: "call_abc".to_string(),
                    command: "time sleep 300".to_string(),
                    cwd: PathBuf::from("/Users/dank/code/xlsynth"),
                    started_at: "2026-04-07T17:59:00Z".to_string(),
                    updated_at: "2026-04-07T18:00:00Z".to_string(),
                    tty: true,
                }],
            },
            owner_watchdogs: SessionStateOwnerWatchdogs::default(),
            subagent: Some(SessionStateSubagent {
                parent_thread_id: ThreadId::try_from("00000000-0000-4000-8000-000000000001")
                    .unwrap(),
                depth: 1,
                edge_status: SessionStateSubagentEdgeStatus::Open,
                agent_path: Some("/root/phase1_correctness_review".to_string()),
                agent_nickname: Some("Parfit".to_string()),
                agent_role: Some("explorer".to_string()),
            }),
        }
    }

    fn subagent_session_source() -> SessionSource {
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: ThreadId::try_from("00000000-0000-4000-8000-0000000000aa").unwrap(),
            depth: 2,
            agent_path: Some(AgentPath::try_from("/root/reviewer").unwrap()),
            agent_nickname: Some("Parfit".to_string()),
            agent_role: Some("explorer".to_string()),
        })
    }

    #[test]
    fn session_state_sidecar_path_uses_rollout_extension() {
        let rollout_path = Path::new("/tmp/sessions/2025/03/09/rollout-thread.jsonl");
        let sidecar_path = session_state_sidecar_path(rollout_path);
        assert_eq!(
            sidecar_path,
            PathBuf::from("/tmp/sessions/2025/03/09/rollout-thread.session-state.json")
        );
    }

    #[test]
    fn write_session_state_sidecar_serializes_terminal_attachment() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let rollout_path = temp_dir.path().join("rollout-thread.jsonl");
        let sidecar = live_turn_sidecar(Some(terminal_attachment(
            TerminalAttachmentProvider::Iterm2,
            Some("w0t1p0"),
            Some("/dev/ttys015"),
        )));
        write_session_state_sidecar(&rollout_path, &sidecar)?;

        let sidecar_path = session_state_sidecar_path(&rollout_path);
        let sidecar: SessionStateSidecar =
            serde_json::from_slice(&fs::read(sidecar_path).context("read sidecar")?)?;
        assert_eq!(sidecar.schema_version, SESSION_STATE_SCHEMA_VERSION);
        assert_eq!(
            sidecar.terminal,
            Some(terminal_attachment(
                TerminalAttachmentProvider::Iterm2,
                Some("w0t1p0"),
                Some("/dev/ttys015"),
            ))
        );
        assert!(chrono::DateTime::parse_from_rfc3339(&sidecar.updated_at).is_ok());
        Ok(())
    }

    #[test]
    fn write_session_state_sidecar_serializes_v2_liveness_graph() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let rollout_path = temp_dir.path().join("rollout-thread.jsonl");
        let sidecar = live_turn_sidecar(/*terminal*/ None);

        write_session_state_sidecar(&rollout_path, &sidecar)?;

        let sidecar_json: serde_json::Value =
            serde_json::from_slice(&fs::read(session_state_sidecar_path(&rollout_path))?)?;
        assert_eq!(
            sidecar_json,
            serde_json::json!({
                "schema_version": 2,
                "updated_at": "2026-04-07T18:00:00Z",
                "terminal": null,
                "session": {
                    "status": "open",
                    "lease_expires_at": "2026-04-07T18:01:00Z",
                },
                "root_turn": {
                    "status": "running",
                    "turn_id": "turn-1",
                    "started_at": "2026-04-07T17:58:00Z",
                    "updated_at": "2026-04-07T18:00:00Z",
                    "lease_expires_at": "2026-04-07T18:01:00Z",
                },
                "background_exec": {
                    "processes": [
                        {
                            "process_id": "42",
                            "call_id": "call_abc",
                            "command": "time sleep 300",
                            "cwd": "/Users/dank/code/xlsynth",
                            "started_at": "2026-04-07T17:59:00Z",
                            "updated_at": "2026-04-07T18:00:00Z",
                            "tty": true,
                        },
                    ],
                },
                "owner_watchdogs": {
                    "active_count": 0,
                },
                "subagent": {
                    "parent_thread_id": "00000000-0000-4000-8000-000000000001",
                    "depth": 1,
                    "edge_status": "open",
                    "agent_path": "/root/phase1_correctness_review",
                    "agent_nickname": "Parfit",
                    "agent_role": "explorer",
                },
            })
        );
        Ok(())
    }

    #[test]
    fn refreshed_idle_sidecar_extends_open_session_lease() {
        let sidecar = idle_sidecar(/*terminal*/ None);
        let refreshed_at = chrono::DateTime::parse_from_rfc3339("2026-04-07T18:00:15Z")
            .unwrap()
            .with_timezone(&Utc);

        let refreshed = refreshed_sidecar(&sidecar, refreshed_at, sidecar.root_turn.clone());

        assert_eq!(refreshed.root_turn, SessionStateRootTurn::Idle);
        assert_eq!(
            refreshed.session,
            SessionStateSession::Open {
                lease_expires_at: "2026-04-07T18:01:15Z".to_string(),
            }
        );
    }

    #[test]
    fn write_session_state_sidecar_writes_terminal_null_when_missing() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let rollout_path = temp_dir.path().join("rollout-thread.jsonl");
        let sidecar = idle_sidecar(/*terminal*/ None);
        write_session_state_sidecar(&rollout_path, &sidecar)?;

        let sidecar_json: serde_json::Value =
            serde_json::from_slice(&fs::read(session_state_sidecar_path(&rollout_path))?)?;
        assert_eq!(sidecar_json["terminal"], serde_json::Value::Null);
        Ok(())
    }

    #[test]
    fn session_state_sidecar_deserializes_v2_without_owner_watchdogs() -> Result<()> {
        let sidecar: SessionStateSidecar = serde_json::from_value(serde_json::json!({
            "schema_version": 2,
            "updated_at": "2026-04-07T18:00:00Z",
            "terminal": null,
            "root_turn": {"status": "idle"},
            "background_exec": {"processes": []},
            "subagent": null,
        }))?;

        assert_eq!(sidecar.owner_watchdogs.active_count, 0);
        assert!(matches!(sidecar.session, SessionStateSession::Open { .. }));
        Ok(())
    }

    #[test]
    fn terminal_attachment_sidecar_refresh_skips_noninteractive_sources() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let rollout_path = temp_dir.path().join("rollout-thread.jsonl");

        let wrote = refresh_interactive_session_state_sidecar(
            Some(&rollout_path),
            &SessionSource::Exec,
            Some(&terminal_attachment(
                TerminalAttachmentProvider::Iterm2,
                Some("w0t1p0"),
                /*tty*/ None,
            )),
        )?;

        assert!(!wrote);
        assert!(!session_state_sidecar_path(&rollout_path).exists());
        Ok(())
    }

    #[test]
    fn session_state_tracker_writes_and_closes_subagent_edge() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let rollout_path = temp_dir.path().join("rollout-subagent.jsonl");
        let tracker = SessionStateTracker::new(
            Some(rollout_path.clone()),
            &subagent_session_source(),
            /*terminal*/ None,
        );

        assert!(tracker.write_current()?);
        let sidecar: SessionStateSidecar =
            serde_json::from_slice(&fs::read(session_state_sidecar_path(&rollout_path))?)?;
        assert_eq!(
            sidecar.subagent.as_ref().map(|subagent| (
                &subagent.edge_status,
                subagent.agent_path.as_deref(),
                subagent.depth,
            )),
            Some((
                &SessionStateSubagentEdgeStatus::Open,
                Some("/root/reviewer"),
                2,
            )),
        );

        tracker.note_root_turn_started("turn-1")?;
        tracker.note_root_turn_completed("turn-1")?;
        let sidecar: SessionStateSidecar =
            serde_json::from_slice(&fs::read(session_state_sidecar_path(&rollout_path))?)?;
        assert_eq!(
            sidecar
                .subagent
                .as_ref()
                .map(|subagent| &subagent.edge_status),
            Some(&SessionStateSubagentEdgeStatus::Closed),
        );
        assert!(matches!(
            sidecar.root_turn,
            SessionStateRootTurn::Completed { .. },
        ));
        Ok(())
    }

    #[test]
    fn session_state_tracker_updates_owner_watchdog_count() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let rollout_path = temp_dir.path().join("rollout-watchdog.jsonl");
        let tracker = SessionStateTracker::new(
            Some(rollout_path.clone()),
            &SessionSource::Cli,
            /*terminal*/ None,
        );

        tracker.write_current()?;
        tracker.set_owner_watchdog_count(2)?;

        let sidecar: SessionStateSidecar =
            serde_json::from_slice(&fs::read(session_state_sidecar_path(&rollout_path))?)?;
        assert_eq!(sidecar.owner_watchdogs.active_count, 2);

        Ok(())
    }

    #[test]
    fn session_state_tracker_closes_session_lifecycle() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let rollout_path = temp_dir.path().join("rollout-session.jsonl");
        let tracker = SessionStateTracker::new(
            Some(rollout_path.clone()),
            &SessionSource::Cli,
            /*terminal*/ None,
        );

        tracker.write_current()?;
        tracker.note_session_closed()?;

        let sidecar: SessionStateSidecar =
            serde_json::from_slice(&fs::read(session_state_sidecar_path(&rollout_path))?)?;
        assert_eq!(sidecar.session, SessionStateSession::Closed);

        Ok(())
    }

    #[test]
    fn session_state_tracker_refreshes_idle_session_lifecycle_lease() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let rollout_path = temp_dir.path().join("rollout-session-refresh.jsonl");
        let tracker = SessionStateTracker::new(
            Some(rollout_path.clone()),
            &SessionSource::Cli,
            /*terminal*/ None,
        );
        let first_observed_at = chrono::DateTime::parse_from_rfc3339("2026-04-07T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let second_observed_at = chrono::DateTime::parse_from_rfc3339("2026-04-07T18:00:15Z")
            .unwrap()
            .with_timezone(&Utc);

        tracker.update_at(first_observed_at, |current, now| {
            refreshed_sidecar(current, now, current.root_turn.clone())
        })?;
        let initial_sidecar: SessionStateSidecar =
            serde_json::from_slice(&fs::read(session_state_sidecar_path(&rollout_path))?)?;

        tracker.update_at(second_observed_at, |current, now| {
            refreshed_sidecar(current, now, current.root_turn.clone())
        })?;
        let refreshed_sidecar: SessionStateSidecar =
            serde_json::from_slice(&fs::read(session_state_sidecar_path(&rollout_path))?)?;

        assert_eq!(
            initial_sidecar,
            build_idle_session_state_sidecar(
                /*terminal*/ None,
                /*subagent*/ None,
                first_observed_at,
            )
        );
        assert_eq!(
            refreshed_sidecar,
            build_idle_session_state_sidecar(
                /*terminal*/ None,
                /*subagent*/ None,
                second_observed_at,
            )
        );

        Ok(())
    }

    #[tokio::test]
    async fn move_session_state_sidecar_if_present_renames_sibling() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let from_rollout_path = temp_dir.path().join("rollout-a.jsonl");
        let to_rollout_path = temp_dir.path().join("rollout-b.jsonl");
        let sidecar = idle_sidecar(Some(terminal_attachment(
            TerminalAttachmentProvider::Iterm2,
            Some("w0t1p0"),
            /*tty*/ None,
        )));
        write_session_state_sidecar(&from_rollout_path, &sidecar)?;

        move_session_state_sidecar_if_present(&from_rollout_path, &to_rollout_path).await?;

        assert!(!session_state_sidecar_path(&from_rollout_path).exists());
        assert!(session_state_sidecar_path(&to_rollout_path).exists());
        Ok(())
    }

    #[tokio::test]
    async fn move_session_state_sidecar_if_present_ignores_missing_source() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let from_rollout_path = temp_dir.path().join("rollout-a.jsonl");
        let to_rollout_path = temp_dir.path().join("rollout-b.jsonl");

        move_session_state_sidecar_if_present(&from_rollout_path, &to_rollout_path).await?;

        assert!(!session_state_sidecar_path(&to_rollout_path).exists());
        Ok(())
    }
}
