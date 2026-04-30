//! Rollout persistence and discovery for Codex session files.

use std::sync::LazyLock;

use codex_protocol::protocol::SessionSource;

pub(crate) mod config;
pub(crate) mod list;
pub(crate) mod metadata;
pub(crate) mod policy;
pub(crate) mod recorder;
pub(crate) mod session_index;
pub(crate) mod session_state;
pub(crate) mod session_state_contract;
pub mod state_db;

pub(crate) mod default_client {
    pub use codex_login::default_client::*;
}

pub(crate) use codex_protocol::protocol;

pub const SESSIONS_SUBDIR: &str = "sessions";
pub const ARCHIVED_SESSIONS_SUBDIR: &str = "archived_sessions";
pub static INTERACTIVE_SESSION_SOURCES: LazyLock<Vec<SessionSource>> = LazyLock::new(|| {
    vec![
        SessionSource::Cli,
        SessionSource::VSCode,
        SessionSource::Custom("atlas".to_string()),
        SessionSource::Custom("chatgpt".to_string()),
    ]
});

pub use codex_protocol::protocol::SessionMeta;
pub use config::Config;
pub use config::RolloutConfig;
pub use config::RolloutConfigView;
pub use list::Cursor;
pub use list::SortDirection;
pub use list::ThreadItem;
pub use list::ThreadListConfig;
pub use list::ThreadListLayout;
pub use list::ThreadSortKey;
pub use list::ThreadsPage;
pub use list::find_archived_thread_path_by_id_str;
pub use list::find_or_unarchive_thread_path_by_id_str;
pub use list::find_thread_path_by_id_str;
#[deprecated(note = "use find_thread_path_by_id_str")]
pub use list::find_thread_path_by_id_str as find_conversation_path_by_id_str;
pub use list::get_threads;
pub use list::get_threads_in_root;
pub use list::parse_cursor;
pub use list::read_head_for_summary;
pub use list::read_session_meta_line;
pub use list::read_thread_item_from_rollout;
pub use list::resolve_fork_reference_rollout_path;
pub use list::rollout_date_parts;
pub use metadata::builder_from_items;
pub use policy::EventPersistenceMode;
pub use policy::should_persist_response_item_for_memories;
pub use recorder::RolloutRecorder;
pub use recorder::RolloutRecorderParams;
pub use recorder::append_rollout_item_to_path;
pub use session_index::append_thread_name;
pub use session_index::find_thread_meta_by_name_str;
pub use session_index::find_thread_name_by_id;
pub use session_index::find_thread_names_by_ids;
pub use session_state::SESSION_HEARTBEAT_SECONDS;
pub use session_state::SessionStateBackgroundExecProcess;
pub use session_state::SessionStateOwnerWatchdogs;
pub use session_state::SessionStateSession;
pub use session_state::SessionStateSidecar;
pub use session_state::SessionStateTracker;
pub use session_state::move_session_state_sidecar_if_present;
pub use session_state::refresh_interactive_session_state_sidecar;
pub use session_state::session_state_sidecar_path;
pub use session_state::write_session_state_sidecar;
pub use session_state_contract::SESSION_STATE_CONTRACT_FIXTURE_DIR;
pub use session_state_contract::write_session_state_contract_fixtures;
pub use state_db::StateDbHandle;

#[cfg(test)]
mod tests;
