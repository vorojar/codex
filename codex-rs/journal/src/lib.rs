//! Typed journal model for prompt rendering, filtering, forking, and persistence.
//!
//! A [`Journal`] stores one append-only sequence of [`JournalEntry`] values and resolves it into
//! two derived views:
//!
//! - prompt metadata: keyed, deduplicated entries ordered by `prompt_order`
//! - transcript: ordered prompt-visible items after checkpoint application
//!
//! Callers can resolve the journal once, then choose how to project those views into prompt
//! messages with [`PromptRenderer`].

mod context_builder;
mod error;
pub mod history;
mod journal;
mod prompt_view;
mod render;

#[cfg(test)]
mod tests;

pub use codex_protocol::journal::JournalCheckpointItem;
pub use codex_protocol::journal::JournalContextAudience;
pub use codex_protocol::journal::JournalContextForkBehavior;
pub use codex_protocol::journal::JournalEntry;
pub use codex_protocol::journal::JournalHistoryCursor;
pub use codex_protocol::journal::JournalItem;
pub use codex_protocol::journal::JournalKey;
pub use codex_protocol::journal::JournalMetadataItem;
pub use codex_protocol::journal::JournalReplacePrefixCheckpoint;
pub use codex_protocol::journal::JournalTranscriptItem;
pub use codex_protocol::journal::JournalTruncateHistoryCheckpoint;
pub use codex_protocol::journal::KeyFilter;
pub use codex_protocol::journal::PromptMessage;
pub use codex_protocol::journal::PromptMessageRole;
pub use context_builder::MetadataEntryBuilder;
pub use error::JournalError;
pub use error::Result;
pub use journal::Journal;
pub use journal::ResolvedJournal;
pub use journal::ResolvedMetadata;
pub use journal::ResolvedTranscript;
pub use prompt_view::PromptView;
pub use render::PromptRenderer;

pub type JournalContextItem = JournalMetadataItem;
pub type JournalHistoryItem = JournalTranscriptItem;
pub type ContextEntryBuilder = MetadataEntryBuilder;
pub type ResolvedContexts = ResolvedMetadata;
pub type ResolvedHistory = ResolvedTranscript;
