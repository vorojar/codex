//! Canonical fixture artifacts for the persisted session-state wire contract.

use crate::session_state::SessionStateBackgroundExec;
use crate::session_state::SessionStateOwnerWatchdogs;
use crate::session_state::SessionStateRootTurn;
use crate::session_state::SessionStateSession;
use crate::session_state::SessionStateSidecar;
use anyhow::Context;
use anyhow::Result;
use std::fs;
use std::path::Path;

pub const SESSION_STATE_CONTRACT_FIXTURE_DIR: &str = "fixtures/session-state";
const OPEN_COMPLETED_FIXTURE: &str = "session-state-v2-open-completed.json";
const CLOSED_COMPLETED_FIXTURE: &str = "session-state-v2-closed-completed.json";

/// Writes the producer-owned session-state contract fixtures under `fixture_dir`.
pub fn write_session_state_contract_fixtures(fixture_dir: &Path) -> Result<()> {
    fs::create_dir_all(fixture_dir)
        .with_context(|| format!("create fixture dir {}", fixture_dir.display()))?;
    for (file_name, sidecar) in contract_fixtures() {
        let mut contents =
            serde_json::to_vec_pretty(&sidecar).context("serialize session-state fixture")?;
        contents.push(b'\n');
        fs::write(fixture_dir.join(file_name), contents)
            .with_context(|| format!("write fixture {file_name}"))?;
    }
    Ok(())
}

fn contract_fixtures() -> [(&'static str, SessionStateSidecar); 2] {
    [
        (OPEN_COMPLETED_FIXTURE, open_completed_sidecar()),
        (CLOSED_COMPLETED_FIXTURE, closed_completed_sidecar()),
    ]
}

fn open_completed_sidecar() -> SessionStateSidecar {
    SessionStateSidecar {
        schema_version: 2,
        updated_at: "2026-04-07T18:00:00Z".to_string(),
        terminal: None,
        session: SessionStateSession::Open {
            lease_expires_at: "2026-04-07T18:01:00Z".to_string(),
        },
        root_turn: completed_root_turn(),
        background_exec: SessionStateBackgroundExec::default(),
        owner_watchdogs: SessionStateOwnerWatchdogs::default(),
        subagent: None,
    }
}

fn closed_completed_sidecar() -> SessionStateSidecar {
    SessionStateSidecar {
        schema_version: 2,
        updated_at: "2026-04-07T18:00:00Z".to_string(),
        terminal: None,
        session: SessionStateSession::Closed,
        root_turn: completed_root_turn(),
        background_exec: SessionStateBackgroundExec::default(),
        owner_watchdogs: SessionStateOwnerWatchdogs::default(),
        subagent: None,
    }
}

fn completed_root_turn() -> SessionStateRootTurn {
    SessionStateRootTurn::Completed {
        turn_id: "turn-1".to_string(),
        started_at: "2026-04-07T17:58:00Z".to_string(),
        completed_at: "2026-04-07T18:00:00Z".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    #[test]
    fn generated_session_state_contract_fixtures_match_checked_in_artifacts() -> Result<()> {
        let fixture_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join(SESSION_STATE_CONTRACT_FIXTURE_DIR);
        let generated_dir = TempDir::new()?;
        write_session_state_contract_fixtures(generated_dir.path())?;

        for (file_name, _) in contract_fixtures() {
            let expected = fs::read_to_string(fixture_dir.join(file_name))
                .with_context(|| format!("read checked-in fixture {file_name}"))?;
            let actual = fs::read_to_string(generated_dir.path().join(file_name))
                .with_context(|| format!("read generated fixture {file_name}"))?;
            assert_eq!(
                expected, actual,
                "session-state fixture drifted; run `just write-session-state-fixtures`"
            );
        }
        Ok(())
    }
}
