# codex-journal

`codex-journal` is the typed journal model behind prompt metadata, transcript rewriting, forked views,
and prompt-ready projection in Codex.

## Model

A `Journal` stores one append-only sequence of `JournalEntry` values. Each entry is one of:

- prompt metadata: keyed entries that can replace older entries with the same key
- transcript: ordered prompt-visible items
- checkpoints: transcript rewrite operations such as prefix replacement or truncation

The append-only journal is the source of truth. Derived views are produced by resolving it.

## Resolved Views

`Journal::resolve()` returns a `ResolvedJournal` with two typed views:

- `ResolvedMetadata`: effective prompt-metadata entries after key-based replacement
- `ResolvedTranscript`: effective transcript after checkpoint application

These two views come from the same journal, but they behave differently:

- metadata is deduplicated by key and ordered by `prompt_order`
- transcript preserves order and is rewritten only by checkpoints

## Building Metadata Entries

Use `Journal::metadata_entry_builder(...)` when you want explicit control over metadata:

```rust
use codex_journal::Journal;
use codex_journal::JournalContextAudience;
use codex_journal::PromptMessage;

let entry = Journal::metadata_entry_builder(
    ["prompt", "developer", "permissions"],
    PromptMessage::developer_text("sandbox is workspace-write"),
)
.prompt_order(20)
.audience(JournalContextAudience::All)
.build();
```

`Journal::metadata_entry(...)` remains available as a shorthand for the common case.

## Rendering

Prompt rendering lives in `PromptRenderer`, not in `Journal` itself. Grouping is explicit and
applied only to consecutive resolved metadata entries:

```rust
use codex_journal::Journal;
use codex_journal::KeyFilter;
use codex_journal::PromptMessage;
use codex_journal::PromptRenderer;

let journal = Journal::from_entries(vec![
    Journal::metadata_entry(
        ["prompt", "developer", "one"],
        10,
        PromptMessage::developer_text("first"),
    ).unwrap(),
    Journal::metadata_entry(
        ["prompt", "developer", "two"],
        20,
        PromptMessage::developer_text("second"),
    ).unwrap(),
]);

let resolved = journal.resolve().unwrap();
let prompt = PromptRenderer::new()
    .group(KeyFilter::prefix(["prompt", "developer"]))
    .render_metadata(resolved.metadata());
```

## Transformations

`Journal` also supports:

- `filter` and `resolve_with_filter` for key-scoped views
- `flatten` for dropping obsolete entries while keeping the current effective view
- `fork` for producing child views with audience and `on_fork` filtering
- `with_history_window` for keeping only a recent hot history suffix
- `persist_jsonl` / `load_jsonl` for durable JSONL storage
