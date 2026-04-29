use crate::JournalContextAudience;
use crate::JournalContextForkBehavior;
use crate::JournalEntry;
use crate::JournalKey;
use crate::JournalMetadataItem;
use crate::PromptMessage;
use codex_protocol::models::ContentItem;

/// Builder for one prompt-metadata journal entry.
#[derive(Debug, Clone)]
pub struct MetadataEntryBuilder {
    key: JournalKey,
    message: PromptMessage,
    prompt_order: i64,
    audience: JournalContextAudience,
    on_fork: JournalContextForkBehavior,
    tags: Vec<String>,
    source: Option<String>,
}

impl MetadataEntryBuilder {
    pub(crate) fn new(key: impl Into<JournalKey>, message: impl Into<PromptMessage>) -> Self {
        Self {
            key: key.into(),
            message: message.into(),
            prompt_order: 0,
            audience: JournalContextAudience::default(),
            on_fork: JournalContextForkBehavior::default(),
            tags: Vec::new(),
            source: None,
        }
    }

    /// Sets the prompt ordering used after resolution.
    pub fn prompt_order(mut self, prompt_order: i64) -> Self {
        self.prompt_order = prompt_order;
        self
    }

    /// Sets the audience used when projecting or forking the journal.
    pub fn audience(mut self, audience: JournalContextAudience) -> Self {
        self.audience = audience;
        self
    }

    /// Sets how this context entry behaves when creating a forked journal view.
    pub fn on_fork(mut self, on_fork: JournalContextForkBehavior) -> Self {
        self.on_fork = on_fork;
        self
    }

    /// Sets arbitrary tags for downstream classification.
    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Records the origin of this prompt-metadata entry.
    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Builds one prompt-metadata entry if the message is non-empty after trimming text content.
    pub fn build(self) -> Option<JournalEntry> {
        if self.message.content.is_empty()
            || self.message.content.iter().all(|item| match item {
                ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                    text.trim().is_empty()
                }
                ContentItem::InputImage { .. } => false,
            })
        {
            return None;
        }

        let mut item = JournalMetadataItem::new(self.message)
            .with_prompt_order(self.prompt_order)
            .with_audience(self.audience)
            .with_on_fork(self.on_fork)
            .with_tags(self.tags);
        if let Some(source) = self.source {
            item = item.with_source(source);
        }
        Some(JournalEntry::new(self.key, item))
    }
}
