//! Discovers subagent threads that belong to a primary thread by walking loaded-thread summaries.
//!
//! When the TUI resumes or switches to an existing thread, it needs to populate
//! `AgentNavigationState` and `ChatWidget` metadata for every subagent that was spawned during
//! that thread's lifetime. The app server exposes a flat summary list of currently loaded threads
//! via `thread/loaded/list`, but the TUI must figure out which of those are descendants of the
//! primary thread.
//!
//! This module provides the pure, synchronous tree-walk that turns that flat list into the filtered
//! set of descendants. It intentionally has no async, no I/O, and no side effects so it can be
//! unit-tested in isolation.
//!
//! The walk starts from `primary_thread_id` and repeatedly follows loaded summary
//! `parent_thread_id` edges until no new children are found. The primary thread itself is never
//! included in the output.

use codex_app_server_protocol::ThreadLoadedSummary;
use codex_protocol::ThreadId;
use std::collections::HashMap;
use std::collections::HashSet;

/// A subagent thread discovered by the spawn-tree walk, carrying just enough metadata for the
/// TUI to register it in the navigation cache and rendering metadata map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoadedSubagentThread {
    pub(crate) thread_id: ThreadId,
    pub(crate) agent_nickname: Option<String>,
    pub(crate) agent_role: Option<String>,
}

/// Walks the spawn tree rooted at `primary_thread_id` and returns every descendant subagent.
///
/// The walk is breadth-first over loaded summary `parent_thread_id` edges. Threads whose
/// `parent_thread_id` does not chain back to `primary_thread_id` are excluded. The primary thread
/// itself is never included.
///
/// Results are sorted by stringified thread id for deterministic output in tests and in the
/// navigation cache. Callers should not rely on this ordering for anything semantic; it exists
/// purely to make snapshot assertions stable.
///
/// If two threads claim the same parent, both are included. Cycles in the parent chain are not
/// possible because `ThreadId`s are server-assigned UUIDs and the server enforces acyclicity, but
/// the `included` set guards against re-visiting regardless.
pub(crate) fn find_loaded_subagent_threads_for_primary(
    summaries: Vec<ThreadLoadedSummary>,
    primary_thread_id: ThreadId,
) -> Vec<LoadedSubagentThread> {
    let mut summaries_by_id = HashMap::new();
    for summary in summaries {
        let Ok(thread_id) = ThreadId::from_string(&summary.id) else {
            continue;
        };
        summaries_by_id.insert(thread_id, summary);
    }

    let mut included = HashSet::new();
    let mut pending = vec![primary_thread_id];
    while let Some(parent_thread_id) = pending.pop() {
        for (thread_id, summary) in &summaries_by_id {
            if included.contains(thread_id) {
                continue;
            }

            let Some(source_parent_thread_id) = summary
                .parent_thread_id
                .as_deref()
                .and_then(|id| ThreadId::from_string(id).ok())
            else {
                continue;
            };

            if source_parent_thread_id != parent_thread_id {
                continue;
            }

            included.insert(*thread_id);
            pending.push(*thread_id);
        }
    }

    let mut loaded_threads: Vec<LoadedSubagentThread> = included
        .into_iter()
        .filter_map(|thread_id| {
            summaries_by_id
                .remove(&thread_id)
                .map(|summary| LoadedSubagentThread {
                    thread_id,
                    agent_nickname: summary.agent_nickname,
                    agent_role: summary.agent_role,
                })
        })
        .collect();
    loaded_threads.sort_by_key(|thread| thread.thread_id.to_string());
    loaded_threads
}

#[cfg(test)]
mod tests {
    use super::LoadedSubagentThread;
    use super::find_loaded_subagent_threads_for_primary;
    use codex_app_server_protocol::ThreadLoadedSummary;
    use codex_protocol::ThreadId;
    use pretty_assertions::assert_eq;

    fn summary(
        thread_id: ThreadId,
        parent_thread_id: Option<ThreadId>,
        agent_nickname: Option<&str>,
        agent_role: Option<&str>,
    ) -> ThreadLoadedSummary {
        ThreadLoadedSummary {
            id: thread_id.to_string(),
            parent_thread_id: parent_thread_id.map(|id| id.to_string()),
            agent_nickname: agent_nickname.map(str::to_string),
            agent_role: agent_role.map(str::to_string),
        }
    }

    #[test]
    fn finds_loaded_subagent_tree_for_primary_thread() {
        let primary_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000001").expect("valid thread");
        let child_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000002").expect("valid thread");
        let grandchild_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000003").expect("valid thread");
        let unrelated_parent_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000004").expect("valid thread");
        let unrelated_child_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000005").expect("valid thread");

        let loaded = find_loaded_subagent_threads_for_primary(
            vec![
                summary(
                    primary_thread_id,
                    /*parent_thread_id*/ None,
                    /*agent_nickname*/ None,
                    /*agent_role*/ None,
                ),
                summary(
                    child_thread_id,
                    Some(primary_thread_id),
                    Some("Scout"),
                    Some("explorer"),
                ),
                summary(
                    grandchild_thread_id,
                    Some(child_thread_id),
                    Some("Atlas"),
                    Some("worker"),
                ),
                summary(
                    unrelated_child_id,
                    Some(unrelated_parent_id),
                    Some("Other"),
                    Some("researcher"),
                ),
            ],
            primary_thread_id,
        );

        assert_eq!(
            loaded,
            vec![
                LoadedSubagentThread {
                    thread_id: child_thread_id,
                    agent_nickname: Some("Scout".to_string()),
                    agent_role: Some("explorer".to_string()),
                },
                LoadedSubagentThread {
                    thread_id: grandchild_thread_id,
                    agent_nickname: Some("Atlas".to_string()),
                    agent_role: Some("worker".to_string()),
                },
            ]
        );
    }
}
