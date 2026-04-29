use crate::JournalEntry;
use crate::JournalError;
use crate::JournalItem;
use crate::JournalKey;
use crate::KeyFilter;
use crate::MetadataEntryBuilder;
use crate::PromptView;
use crate::Result;
use codex_protocol::journal::JournalCheckpointItem;
use codex_protocol::journal::JournalContextAudience;
use codex_protocol::journal::JournalContextForkBehavior;
use codex_protocol::journal::JournalHistoryCursor;
use codex_protocol::journal::JournalMetadataItem;
use codex_protocol::journal::JournalReplacePrefixCheckpoint;
use codex_protocol::journal::JournalTranscriptItem;
use codex_protocol::journal::JournalTruncateHistoryCheckpoint;
use codex_protocol::models::ResponseItem;
use indexmap::IndexMap;
use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;
use std::io::BufWriter;
use std::io::Write;
use std::path::Path;

/// Canonical typed journal.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Journal {
    entries: Vec<JournalEntry>,
}

impl Journal {
    /// Creates an empty journal.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a journal from an explicit list of entries.
    pub fn from_entries(entries: Vec<JournalEntry>) -> Self {
        Self { entries }
    }

    /// Appends one keyed item to the journal.
    pub fn add<K, T>(&mut self, key: K, item: T)
    where
        K: Into<JournalKey>,
        T: Into<JournalItem>,
    {
        self.entries.push(JournalEntry::new(key, item));
    }

    /// Appends several keyed journal entries.
    pub fn extend<I, T>(&mut self, entries: I)
    where
        I: IntoIterator<Item = T>,
        T: Into<JournalEntry>,
    {
        self.entries.extend(entries.into_iter().map(Into::into));
    }

    /// Returns the raw append-only journal entries.
    pub fn entries(&self) -> &[JournalEntry] {
        &self.entries
    }

    /// Starts building one prompt-metadata entry.
    pub fn metadata_entry_builder(
        key: impl Into<JournalKey>,
        message: impl Into<crate::PromptMessage>,
    ) -> MetadataEntryBuilder {
        MetadataEntryBuilder::new(key, message)
    }

    /// Builds one prompt-metadata entry if the message is non-empty after trimming text content.
    pub fn metadata_entry(
        key: impl Into<JournalKey>,
        prompt_order: i64,
        message: impl Into<crate::PromptMessage>,
    ) -> Option<JournalEntry> {
        Self::metadata_entry_builder(key, message)
            .prompt_order(prompt_order)
            .build()
    }

    pub fn context_entry_builder(
        key: impl Into<JournalKey>,
        message: impl Into<crate::PromptMessage>,
    ) -> MetadataEntryBuilder {
        Self::metadata_entry_builder(key, message)
    }

    pub fn context_entry(
        key: impl Into<JournalKey>,
        prompt_order: i64,
        message: impl Into<crate::PromptMessage>,
    ) -> Option<JournalEntry> {
        Self::metadata_entry(key, prompt_order, message)
    }

    /// Returns a journal containing only journal entries whose keys match the filter.
    pub fn filter(&self, filter: &KeyFilter) -> Self {
        let entries = self
            .entries
            .iter()
            .filter(|entry| filter.matches(&entry.key))
            .cloned()
            .collect();
        Self::from_entries(entries)
    }

    /// Returns the number of journal entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether the journal is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Renders the current effective journal view into model prompt items.
    pub fn to_prompt(&self, view: &PromptView) -> Result<Vec<ResponseItem>> {
        self.to_prompt_matching_filter(view, None)
    }

    /// Renders the current effective journal view into model prompt items after selecting only
    /// journal entries whose keys match the filter.
    pub fn to_prompt_with_filter(
        &self,
        view: &PromptView,
        filter: &KeyFilter,
    ) -> Result<Vec<ResponseItem>> {
        self.to_prompt_matching_filter(view, Some(filter))
    }

    /// Produces a flattened child state for the provided view.
    pub fn fork(&self, view: &PromptView) -> Result<Self> {
        self.fork_matching_filter(view, None)
    }

    /// Produces a flattened child state for the provided view after selecting only
    /// journal entries whose keys match the filter.
    pub fn fork_with_filter(&self, view: &PromptView, filter: &KeyFilter) -> Result<Self> {
        self.fork_matching_filter(view, Some(filter))
    }

    /// Drops obsolete journal entries and keeps only the current effective journal view.
    ///
    /// This is the first building block for a rolling in-memory window: callers can
    /// persist the full journal elsewhere, then keep only the flattened journal hot.
    pub fn flatten(&self) -> Result<Self> {
        let resolved = self.resolve()?;
        Ok(Self::from_entries(resolved.into_entries()))
    }

    /// Keeps only the current effective journal view plus the history suffix that starts
    /// at the resolved cursor.
    ///
    /// This is a lightweight rolling-window helper: callers can persist the full
    /// journal on disk, then keep only a recent hot suffix in memory.
    pub fn with_history_window(&self, start: &JournalHistoryCursor) -> Result<Self> {
        let resolved = self.resolve()?;
        let start_index = resolve_cursor(resolved.transcript().entries(), start)?;
        let ResolvedJournal {
            metadata,
            transcript,
        } = resolved;
        Ok(Self::from_entries(
            metadata
                .into_iter()
                .chain(transcript.into_iter().skip(start_index))
                .collect(),
        ))
    }

    /// Persists the raw journal to a JSONL file, one `JournalEntry` per line.
    pub fn persist_jsonl(&self, path: &Path) -> Result<()> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);
        for entry in &self.entries {
            serde_json::to_writer(&mut writer, entry)?;
            writer.write_all(b"\n")?;
        }
        writer.flush()?;
        Ok(())
    }

    /// Loads a raw journal from a JSONL file written by [`Self::persist_jsonl`].
    pub fn load_jsonl(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        for (line_index, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let entry = serde_json::from_str::<JournalEntry>(&line).map_err(|source| {
                JournalError::ParseJson {
                    line_number: line_index + 1,
                    source,
                }
            })?;
            entries.push(entry);
        }
        Ok(Self::from_entries(entries))
    }

    /// Resolves the current effective journal into prompt metadata and transcript views.
    pub fn resolve(&self) -> Result<ResolvedJournal> {
        self.resolve_filter(None)
    }

    /// Resolves the current effective journal after selecting only entries whose keys match the
    /// filter.
    pub fn resolve_with_filter(&self, filter: &KeyFilter) -> Result<ResolvedJournal> {
        self.resolve_filter(Some(filter))
    }

    fn to_prompt_matching_filter(
        &self,
        view: &PromptView,
        filter: Option<&KeyFilter>,
    ) -> Result<Vec<ResponseItem>> {
        let resolved = self.resolve_filter(filter)?;
        let mut prompt: Vec<ResponseItem> = resolved
            .metadata
            .into_iter()
            .filter(|entry| metadata_visible_in_view(metadata_item(entry), view))
            .map(|entry| match entry.item {
                JournalItem::Metadata(item) => ResponseItem::from(item),
                _ => unreachable!("resolved metadata entries must be metadata items"),
            })
            .collect();
        prompt.extend(
            resolved
                .transcript
                .into_iter()
                .map(|entry| match entry.item {
                    JournalItem::Transcript(item) => ResponseItem::from(item),
                    _ => unreachable!("resolved transcript entries must be transcript items"),
                }),
        );
        Ok(prompt)
    }

    fn fork_matching_filter(&self, view: &PromptView, filter: Option<&KeyFilter>) -> Result<Self> {
        let resolved = self.resolve_filter(filter)?;
        Ok(Self::from_entries(
            resolved
                .metadata
                .into_iter()
                .filter(|entry| metadata_item(entry).on_fork == JournalContextForkBehavior::Keep)
                .filter(|entry| metadata_visible_in_view(metadata_item(entry), view))
                .chain(resolved.transcript)
                .collect(),
        ))
    }

    fn resolve_filter(&self, filter: Option<&KeyFilter>) -> Result<ResolvedJournal> {
        let mut transcript = Vec::<JournalEntry>::new();
        let mut latest_metadata_by_key = IndexMap::<JournalKey, (usize, JournalEntry)>::new();

        for (index, entry) in self.entries.iter().enumerate() {
            if let Some(filter) = filter
                && !filter.matches(&entry.key)
            {
                continue;
            }
            match &entry.item {
                JournalItem::Transcript(_) => transcript.push(entry.clone()),
                JournalItem::Metadata(_) => {
                    latest_metadata_by_key.insert(entry.key.clone(), (index, entry.clone()));
                }
                JournalItem::Checkpoint(checkpoint) => {
                    apply_checkpoint(&mut transcript, &entry.key, checkpoint)?;
                }
            }
        }

        let mut metadata = latest_metadata_by_key.into_values().collect::<Vec<_>>();
        metadata.sort_by(|(left_index, left_entry), (right_index, right_entry)| {
            metadata_item(left_entry)
                .prompt_order
                .cmp(&metadata_item(right_entry).prompt_order)
                .then_with(|| left_index.cmp(right_index))
        });

        Ok(ResolvedJournal::new(
            metadata.into_iter().map(|(_, entry)| entry).collect(),
            transcript,
        ))
    }
}

/// Effective journal view after deduplicating prompt context and applying history checkpoints.
///
/// `contexts` and `history` are derived from the same append-only source of truth, but they have
/// different semantics:
///
/// - [`ResolvedMetadata`] contains keyed prompt-metadata entries. Later entries with the same key
///   replace earlier ones and the result is ordered by `prompt_order`.
/// - [`ResolvedTranscript`] contains ordered transcript entries after checkpoint application.
///   Transcript preserves order and can be truncated or rewritten by checkpoints.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedJournal {
    metadata: ResolvedMetadata,
    transcript: ResolvedTranscript,
}

impl ResolvedJournal {
    fn new(metadata: Vec<JournalEntry>, transcript: Vec<JournalEntry>) -> Self {
        Self {
            metadata: ResolvedMetadata::new(metadata),
            transcript: ResolvedTranscript::new(transcript),
        }
    }

    /// Returns the effective prompt-metadata view.
    pub fn metadata(&self) -> &ResolvedMetadata {
        &self.metadata
    }

    /// Returns the effective transcript view.
    pub fn transcript(&self) -> &ResolvedTranscript {
        &self.transcript
    }

    /// Returns the effective prompt-metadata view.
    pub fn contexts(&self) -> &ResolvedMetadata {
        self.metadata()
    }

    /// Returns the effective transcript view.
    pub fn history(&self) -> &ResolvedTranscript {
        self.transcript()
    }

    /// Consumes the resolved view and returns only the prompt-metadata entries.
    pub fn into_metadata(self) -> ResolvedMetadata {
        self.metadata
    }

    /// Consumes the resolved view and returns only the transcript entries.
    pub fn into_transcript(self) -> ResolvedTranscript {
        self.transcript
    }

    pub fn into_contexts(self) -> ResolvedMetadata {
        self.into_metadata()
    }

    pub fn into_history(self) -> ResolvedTranscript {
        self.into_transcript()
    }

    /// Consumes the resolved view and concatenates metadata followed by transcript entries.
    pub fn into_entries(self) -> Vec<JournalEntry> {
        self.metadata
            .into_entries()
            .into_iter()
            .chain(self.transcript.into_entries())
            .collect()
    }
}

/// Effective prompt-metadata entries derived from a journal.
///
/// These entries are deduplicated by key and sorted for prompt rendering. They are suitable for
/// rendering or for building flattened and forked journal states.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedMetadata {
    entries: Vec<JournalEntry>,
}

impl ResolvedMetadata {
    fn new(entries: Vec<JournalEntry>) -> Self {
        Self { entries }
    }

    /// Returns the resolved prompt-metadata entries.
    pub fn entries(&self) -> &[JournalEntry] {
        &self.entries
    }

    /// Returns whether there are no resolved prompt-metadata entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the number of resolved prompt-metadata entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Consumes the view and returns its entries.
    pub fn into_entries(self) -> Vec<JournalEntry> {
        self.entries
    }
}

impl IntoIterator for ResolvedMetadata {
    type Item = JournalEntry;
    type IntoIter = std::vec::IntoIter<JournalEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

/// Effective transcript entries derived from a journal.
///
/// These entries preserve prompt-visible order after checkpoint application. Unlike prompt
/// metadata, transcript is not deduplicated by key.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTranscript {
    entries: Vec<JournalEntry>,
}

impl ResolvedTranscript {
    fn new(entries: Vec<JournalEntry>) -> Self {
        Self { entries }
    }

    /// Returns the resolved transcript entries.
    pub fn entries(&self) -> &[JournalEntry] {
        &self.entries
    }

    /// Returns whether there are no resolved transcript entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the number of resolved transcript entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Consumes the view and returns its entries.
    pub fn into_entries(self) -> Vec<JournalEntry> {
        self.entries
    }
}

impl IntoIterator for ResolvedTranscript {
    type Item = JournalEntry;
    type IntoIter = std::vec::IntoIter<JournalEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

fn apply_checkpoint(
    transcript: &mut Vec<JournalEntry>,
    checkpoint_key: &JournalKey,
    checkpoint: &JournalCheckpointItem,
) -> Result<()> {
    match checkpoint {
        JournalCheckpointItem::ReplacePrefix(JournalReplacePrefixCheckpoint {
            through,
            replacement,
        }) => {
            let keep_from = resolve_cursor(transcript.as_slice(), through)?;
            let mut next_transcript =
                Vec::with_capacity(replacement.len() + transcript.len().saturating_sub(keep_from));
            next_transcript.extend(
                replacement
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, item)| {
                        JournalEntry::new(
                            replacement_transcript_key(checkpoint_key, index, &item),
                            item,
                        )
                    }),
            );
            next_transcript.extend(transcript[keep_from..].iter().cloned());
            *transcript = next_transcript;
            Ok(())
        }
        JournalCheckpointItem::TruncateHistory(JournalTruncateHistoryCheckpoint { through }) => {
            let keep_len = resolve_cursor(transcript.as_slice(), through)?;
            transcript.truncate(keep_len);
            Ok(())
        }
    }
}

fn resolve_cursor(transcript: &[JournalEntry], cursor: &JournalHistoryCursor) -> Result<usize> {
    match cursor {
        JournalHistoryCursor::Start => Ok(0),
        JournalHistoryCursor::End => Ok(transcript.len()),
        JournalHistoryCursor::AfterItem(history_item_id) => transcript
            .iter()
            .position(|entry| transcript_item(entry).id == *history_item_id)
            .map(|index| index + 1)
            .ok_or_else(|| JournalError::UnknownHistoryItemId {
                history_item_id: history_item_id.clone(),
            }),
    }
}

fn replacement_transcript_key(
    checkpoint_key: &JournalKey,
    index: usize,
    transcript_item: &JournalTranscriptItem,
) -> JournalKey {
    checkpoint_key
        .child("replacement")
        .child(index.to_string())
        .child(transcript_item.id.clone())
}

fn metadata_item(entry: &JournalEntry) -> &JournalMetadataItem {
    match &entry.item {
        JournalItem::Metadata(item) => item,
        _ => unreachable!("resolved metadata entries must be metadata items"),
    }
}

fn transcript_item(entry: &JournalEntry) -> &JournalTranscriptItem {
    match &entry.item {
        JournalItem::Transcript(item) => item,
        _ => unreachable!("resolved transcript entries must be transcript items"),
    }
}

fn metadata_visible_in_view(item: &JournalMetadataItem, view: &PromptView) -> bool {
    match &item.audience {
        JournalContextAudience::All => true,
        JournalContextAudience::RootOnly => view.is_root,
        JournalContextAudience::SubAgentsOnly => !view.is_root,
        JournalContextAudience::AgentPathPrefix(prefix) => view
            .agent_path
            .as_deref()
            .is_some_and(|agent_path| agent_path.starts_with(prefix)),
        JournalContextAudience::AgentRole(role) => view
            .agent_role
            .as_deref()
            .is_some_and(|agent_role| agent_role == role),
    }
}
