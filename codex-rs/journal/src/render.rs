use crate::JournalItem;
use crate::KeyFilter;
use crate::PromptMessage;
use crate::PromptMessageRole;
use crate::ResolvedMetadata;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;

/// Renders resolved prompt metadata into model-visible prompt messages.
///
/// Group filters are applied in declaration order. Entries are merged only when they are
/// consecutive, match the same group filter, and share the same role.
#[derive(Debug, Clone, Default)]
pub struct PromptRenderer {
    group_filters: Vec<KeyFilter>,
}

impl PromptRenderer {
    /// Creates a renderer with no grouping rules.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one grouping rule for consecutive resolved metadata entries.
    pub fn group(mut self, filter: KeyFilter) -> Self {
        self.group_filters.push(filter);
        self
    }

    /// Renders resolved prompt metadata according to the configured grouping rules.
    pub fn render_metadata(&self, metadata: &ResolvedMetadata) -> Vec<ResponseItem> {
        let mut rendered = Vec::new();
        let mut current_group: Option<usize> = None;
        let mut current_role: Option<PromptMessageRole> = None;
        let mut current_content = Vec::new();

        for entry in metadata.entries() {
            let JournalItem::Metadata(item) = &entry.item else {
                continue;
            };
            let group = self
                .group_filters
                .iter()
                .position(|filter| filter.matches(&entry.key));

            if group.is_none() {
                flush_prompt_message(&mut rendered, &mut current_role, &mut current_content);
                rendered.push(ResponseItem::from(item.clone()));
                current_group = None;
                continue;
            }

            if current_group != group || current_role != Some(item.message.role) {
                flush_prompt_message(&mut rendered, &mut current_role, &mut current_content);
                current_group = group;
                current_role = Some(item.message.role);
            }

            current_content.extend(item.message.content.clone());
        }

        flush_prompt_message(&mut rendered, &mut current_role, &mut current_content);
        rendered
    }

    pub fn render_contexts(&self, contexts: &ResolvedMetadata) -> Vec<ResponseItem> {
        self.render_metadata(contexts)
    }
}

fn flush_prompt_message(
    rendered: &mut Vec<ResponseItem>,
    role: &mut Option<PromptMessageRole>,
    content: &mut Vec<ContentItem>,
) {
    let Some(role) = role.take() else {
        return;
    };
    if content.is_empty() {
        return;
    }
    rendered.push(ResponseItem::from(PromptMessage::new(
        role,
        std::mem::take(content),
    )));
}
