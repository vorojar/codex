use anyhow::Result;
use codex_rollout::SESSION_STATE_CONTRACT_FIXTURE_DIR;
use codex_rollout::write_session_state_contract_fixtures;
use std::path::PathBuf;

fn main() -> Result<()> {
    let fixture_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SESSION_STATE_CONTRACT_FIXTURE_DIR);
    write_session_state_contract_fixtures(&fixture_dir)
}
