use crate::Journal;
use crate::JournalCheckpointItem;
use crate::JournalContextAudience;
use crate::JournalContextForkBehavior;
use crate::JournalEntry;
use crate::JournalHistoryCursor;
use crate::JournalMetadataItem;
use crate::JournalReplacePrefixCheckpoint;
use crate::JournalTranscriptItem;
use crate::JournalTruncateHistoryCheckpoint;
use crate::KeyFilter;
use crate::PromptMessage;
use crate::PromptRenderer;
use crate::PromptView;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

fn user_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

fn assistant_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

fn developer_context(text: &str, prompt_order: i64) -> JournalMetadataItem {
    JournalMetadataItem::new(PromptMessage::developer_text(text)).with_prompt_order(prompt_order)
}

#[test]
fn to_prompt_uses_latest_context_for_key() {
    let mut state = Journal::new();
    state.add(
        ["prompt", "permissions"],
        developer_context("older permissions", 10),
    );
    state.add(["history", "hello"], user_message("hello"));
    state.add(
        ["prompt", "permissions"],
        developer_context("newer permissions", 10),
    );

    let prompt = state
        .to_prompt(&PromptView::root())
        .expect("prompt should render");

    assert_eq!(
        prompt,
        vec![
            ResponseItem::from(PromptMessage::developer_text("newer permissions")),
            user_message("hello"),
        ]
    );
}

#[test]
fn to_prompt_filters_context_by_audience() {
    let mut state = Journal::new();
    state.add(
        ["prompt", "root", "hint"],
        developer_context("root-only", 0).with_audience(JournalContextAudience::RootOnly),
    );
    state.add(
        ["prompt", "child", "hint"],
        developer_context("child-only", 1).with_audience(JournalContextAudience::SubAgentsOnly),
    );

    let root_prompt = state
        .to_prompt(&PromptView::root())
        .expect("root prompt should render");
    let child_prompt = state
        .to_prompt(&PromptView::subagent(
            "/root/worker",
            Option::<String>::None,
        ))
        .expect("child prompt should render");

    assert_eq!(
        root_prompt,
        vec![ResponseItem::from(PromptMessage::developer_text(
            "root-only"
        ))]
    );
    assert_eq!(
        child_prompt,
        vec![ResponseItem::from(PromptMessage::developer_text(
            "child-only"
        ))]
    );
}

#[test]
fn to_prompt_with_filter_matches_key_prefix() {
    let mut state = Journal::new();
    state.add(["prompt", "root", "keep"], developer_context("keep me", 0));
    state.add(["prompt", "child", "drop"], developer_context("drop me", 1));

    let prompt = state
        .to_prompt_with_filter(&PromptView::root(), &KeyFilter::prefix(["prompt", "root"]))
        .expect("prompt should render");

    assert_eq!(
        prompt,
        vec![ResponseItem::from(PromptMessage::developer_text("keep me"))]
    );
}

#[test]
fn checkpoints_replace_prefix_and_then_truncate_history() {
    let first = JournalTranscriptItem::new(user_message("turn 1"));
    let second = JournalTranscriptItem::new(assistant_message("turn 1 answer"));
    let third = JournalTranscriptItem::new(user_message("turn 2"));
    let summary = JournalTranscriptItem {
        id: "summary".to_string(),
        turn_id: None,
        item: assistant_message("summary"),
    };

    let state = Journal::from_entries(vec![
        JournalEntry::new(["history", "1"], first),
        JournalEntry::new(["history", "2"], second.clone()),
        JournalEntry::new(["history", "3"], third),
        JournalEntry::new(
            ["checkpoint", "replace"],
            JournalCheckpointItem::ReplacePrefix(JournalReplacePrefixCheckpoint {
                through: JournalHistoryCursor::AfterItem(second.id),
                replacement: vec![summary.clone()],
            }),
        ),
        JournalEntry::new(
            ["checkpoint", "truncate"],
            JournalCheckpointItem::TruncateHistory(JournalTruncateHistoryCheckpoint {
                through: JournalHistoryCursor::AfterItem(summary.id),
            }),
        ),
    ]);

    let prompt = state
        .to_prompt(&PromptView::root())
        .expect("prompt should render");

    assert_eq!(prompt, vec![assistant_message("summary")]);
}

#[test]
fn flatten_preserves_prompt_and_drops_obsolete_items() {
    let first = JournalTranscriptItem::new(user_message("turn 1"));
    let answer = JournalTranscriptItem::new(assistant_message("turn 1 answer"));
    let summary = JournalTranscriptItem {
        id: "summary".to_string(),
        turn_id: None,
        item: assistant_message("summary"),
    };
    let state = Journal::from_entries(vec![
        JournalEntry::new(["prompt", "permissions"], developer_context("old", 0)),
        JournalEntry::new(["prompt", "permissions"], developer_context("new", 0)),
        JournalEntry::new(["history", "1"], first),
        JournalEntry::new(["history", "2"], answer.clone()),
        JournalEntry::new(
            ["checkpoint", "replace"],
            JournalCheckpointItem::ReplacePrefix(JournalReplacePrefixCheckpoint {
                through: JournalHistoryCursor::AfterItem(answer.id),
                replacement: vec![summary.clone()],
            }),
        ),
    ]);

    let before = state
        .to_prompt(&PromptView::root())
        .expect("prompt should render");
    let flattened = state.flatten().expect("flatten should succeed");
    let after = flattened
        .to_prompt(&PromptView::root())
        .expect("flattened prompt should render");

    assert_eq!(before, after);
    assert_eq!(
        flattened.entries(),
        vec![
            JournalEntry::new(["prompt", "permissions"], developer_context("new", 0),),
            JournalEntry::new(
                ["checkpoint", "replace", "replacement", "0", "summary"],
                summary,
            ),
        ]
    );
}

#[test]
fn with_history_window_keeps_only_recent_effective_history() {
    let first = JournalTranscriptItem::new(user_message("turn 1"));
    let second = JournalTranscriptItem::new(assistant_message("turn 1 answer"));
    let third = JournalTranscriptItem::new(user_message("turn 2"));
    let state = Journal::from_entries(vec![
        JournalEntry::new(
            ["prompt", "permissions", "current"],
            developer_context("p", 0),
        ),
        JournalEntry::new(["history", "1"], first),
        JournalEntry::new(["history", "2"], second.clone()),
        JournalEntry::new(["history", "3"], third.clone()),
    ]);

    let windowed = state
        .with_history_window(&JournalHistoryCursor::AfterItem(second.id))
        .expect("windowing should succeed");

    assert_eq!(
        windowed.entries(),
        vec![
            JournalEntry::new(
                ["prompt", "permissions", "current"],
                developer_context("p", 0),
            ),
            JournalEntry::new(["history", "3"], third),
        ]
    );
}

#[test]
fn fork_drops_non_keep_context_and_respects_audience() {
    let history = JournalTranscriptItem::new(user_message("hello"));
    let state = Journal::from_entries(vec![
        JournalEntry::new(
            ["prompt", "child", "shared"],
            developer_context("shared child context", 0)
                .with_audience(JournalContextAudience::SubAgentsOnly),
        ),
        JournalEntry::new(
            ["prompt", "child", "regenerate"],
            developer_context("usage hint", 1)
                .with_audience(JournalContextAudience::SubAgentsOnly)
                .with_on_fork(JournalContextForkBehavior::Regenerate),
        ),
        JournalEntry::new(
            ["prompt", "root", "only"],
            developer_context("root only", 2).with_audience(JournalContextAudience::RootOnly),
        ),
        JournalEntry::new(["history", "hello"], history.clone()),
    ]);

    let forked = state
        .fork(&PromptView::subagent(
            "/root/worker",
            Option::<String>::None,
        ))
        .expect("fork should succeed");

    assert_eq!(
        forked.entries(),
        vec![
            JournalEntry::new(
                ["prompt", "child", "shared"],
                developer_context("shared child context", 0)
                    .with_audience(JournalContextAudience::SubAgentsOnly),
            ),
            JournalEntry::new(["history", "hello"], history),
        ]
    );
}

#[test]
fn persist_and_load_jsonl_round_trip() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("journal.jsonl");
    let history = JournalTranscriptItem::new(user_message("hello"));
    let state = Journal::from_entries(vec![
        JournalEntry::new(
            ["prompt", "permissions", "current"],
            developer_context("p", 0),
        ),
        JournalEntry::new(["history", "hello"], history),
    ]);

    state
        .persist_jsonl(path.as_path())
        .expect("journal should persist");
    let loaded = Journal::load_jsonl(path.as_path()).expect("journal should load");

    assert_eq!(loaded, state);
}

#[test]
fn filter_returns_matching_raw_entries() {
    let state = Journal::from_entries(vec![
        JournalEntry::new(["prompt", "root", "keep"], developer_context("keep me", 0)),
        JournalEntry::new(["prompt", "child", "drop"], developer_context("drop me", 1)),
        JournalEntry::new(["history", "hello"], user_message("hello")),
    ]);

    let filtered = state.filter(&KeyFilter::prefix(["prompt", "root"]));

    assert_eq!(
        filtered.entries(),
        vec![JournalEntry::new(
            ["prompt", "root", "keep"],
            developer_context("keep me", 0),
        )]
    );
}

#[test]
fn context_entry_skips_blank_text_messages() {
    assert_eq!(
        Journal::context_entry(
            ["prompt", "developer", "blank"],
            10,
            PromptMessage::developer_text("   "),
        ),
        None
    );
}

#[test]
fn context_entry_builder_carries_optional_fields() {
    let entry = Journal::context_entry_builder(
        ["prompt", "developer", "one"],
        PromptMessage::developer_text("first"),
    )
    .prompt_order(10)
    .audience(JournalContextAudience::SubAgentsOnly)
    .on_fork(JournalContextForkBehavior::Regenerate)
    .tags(vec!["foo".to_string(), "bar".to_string()])
    .source("unit-test")
    .build()
    .expect("entry should be kept");

    assert_eq!(
        entry,
        JournalEntry::new(
            ["prompt", "developer", "one"],
            JournalMetadataItem::new(PromptMessage::developer_text("first"))
                .with_prompt_order(10)
                .with_audience(JournalContextAudience::SubAgentsOnly)
                .with_on_fork(JournalContextForkBehavior::Regenerate)
                .with_tags(vec!["foo".to_string(), "bar".to_string()])
                .with_source("unit-test"),
        )
    );
}

#[test]
fn resolve_splits_contexts_from_history() {
    let history = JournalTranscriptItem::new(user_message("hello"));
    let journal = Journal::from_entries(vec![
        Journal::context_entry(
            ["prompt", "developer", "one"],
            10,
            PromptMessage::developer_text("first"),
        )
        .expect("entry should be kept"),
        Journal::context_entry(
            ["prompt", "guardian", "one"],
            20,
            PromptMessage::developer_text("guardian one"),
        )
        .expect("entry should be kept"),
        Journal::context_entry(
            ["prompt", "guardian", "two"],
            30,
            PromptMessage::developer_text("guardian two"),
        )
        .expect("entry should be kept"),
        JournalEntry::new(["history", "hello"], history.clone()),
    ]);

    let resolved = journal.resolve().expect("journal should resolve");

    assert_eq!(
        resolved.metadata().entries(),
        vec![
            Journal::context_entry(
                ["prompt", "developer", "one"],
                10,
                PromptMessage::developer_text("first"),
            )
            .expect("entry should be kept"),
            Journal::context_entry(
                ["prompt", "guardian", "one"],
                20,
                PromptMessage::developer_text("guardian one"),
            )
            .expect("entry should be kept"),
            Journal::context_entry(
                ["prompt", "guardian", "two"],
                30,
                PromptMessage::developer_text("guardian two"),
            )
            .expect("entry should be kept"),
        ]
    );
    assert_eq!(
        resolved.transcript().entries(),
        vec![JournalEntry::new(["history", "hello"], history)]
    );
}

#[test]
fn prompt_renderer_groups_by_declared_prefix_and_role() {
    let resolved = Journal::from_entries(vec![
        Journal::context_entry(
            ["prompt", "developer", "one"],
            10,
            PromptMessage::developer_text("first"),
        )
        .expect("entry should be kept"),
        Journal::context_entry(
            ["prompt", "developer", "two"],
            20,
            PromptMessage::developer_text("second"),
        )
        .expect("entry should be kept"),
        Journal::context_entry(
            ["prompt", "contextual_user", "one"],
            30,
            PromptMessage::user_text("third"),
        )
        .expect("entry should be kept"),
    ])
    .resolve()
    .expect("entries should resolve");

    let rendered = PromptRenderer::new()
        .group(KeyFilter::prefix(["prompt", "developer"]))
        .group(KeyFilter::prefix(["prompt", "contextual_user"]))
        .render_metadata(resolved.metadata());

    assert_eq!(
        rendered,
        vec![
            ResponseItem::Message {
                id: None,
                role: "developer".to_string(),
                content: vec![
                    ContentItem::InputText {
                        text: "first".to_string(),
                    },
                    ContentItem::InputText {
                        text: "second".to_string(),
                    },
                ],
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "third".to_string(),
                }],
                phase: None,
            },
        ]
    );
}

#[test]
fn prompt_renderer_leaves_ungrouped_entries_separate() {
    let resolved = Journal::from_entries(vec![
        Journal::context_entry(
            ["prompt", "developer", "one"],
            10,
            PromptMessage::developer_text("first"),
        )
        .expect("entry should be kept"),
        Journal::context_entry(
            ["prompt", "guardian", "one"],
            20,
            PromptMessage::developer_text("guardian one"),
        )
        .expect("entry should be kept"),
        Journal::context_entry(
            ["prompt", "guardian", "two"],
            30,
            PromptMessage::developer_text("guardian two"),
        )
        .expect("entry should be kept"),
    ])
    .resolve()
    .expect("entries should resolve");

    let rendered = PromptRenderer::new()
        .group(KeyFilter::prefix(["prompt", "developer"]))
        .render_metadata(resolved.metadata());

    assert_eq!(
        rendered,
        vec![
            ResponseItem::Message {
                id: None,
                role: "developer".to_string(),
                content: vec![ContentItem::InputText {
                    text: "first".to_string(),
                }],
                phase: None,
            },
            ResponseItem::from(PromptMessage::developer_text("guardian one")),
            ResponseItem::from(PromptMessage::developer_text("guardian two")),
        ]
    );
}
