use crate::event_mapping::has_non_contextual_dev_message_content;
use crate::event_mapping::is_contextual_dev_message_content;
use crate::event_mapping::is_contextual_user_message_content;
use crate::session::turn_context::TurnContext;
use codex_journal::Journal;
use codex_journal::JournalItem;
use codex_journal::JournalKey;
use codex_journal::JournalTranscriptItem;
use codex_journal::history as thread_history;
use codex_journal::history::estimate_item_token_count;
use codex_journal::history::estimate_response_item_model_visible_bytes;
use codex_journal::history::is_api_message;
use codex_journal::history::is_model_generated_item;
use codex_journal::history::is_user_turn_boundary;
use codex_journal::history::user_turn_boundary_positions;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::InputModality;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TokenUsageInfo;
use codex_protocol::protocol::TurnContextItem;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::approx_token_count;
use std::ops::Deref;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TotalTokenUsageBreakdown {
    pub(crate) last_api_response_total_tokens: i64,
    pub(crate) all_history_items_model_visible_bytes: i64,
    pub(crate) estimated_tokens_of_items_added_since_last_successful_api_response: i64,
    pub(crate) estimated_bytes_of_items_added_since_last_successful_api_response: i64,
}

pub(crate) fn record_items<I>(journal: &mut Journal, items: I, policy: TruncationPolicy)
where
    I: IntoIterator,
    I::Item: Deref<Target = ResponseItem>,
{
    for item in items {
        let item_ref = item.deref();
        if !is_api_message(item_ref) {
            continue;
        }

        let processed = thread_history::truncate_history_item(item_ref, policy);
        push_history_item(journal, processed);
    }
}

pub(crate) fn raw_items(journal: &Journal) -> Vec<ResponseItem> {
    journal
        .entries()
        .iter()
        .filter_map(|entry| match &entry.item {
            JournalItem::Transcript(item) => Some(item.item.clone()),
            JournalItem::Metadata(_) | JournalItem::Checkpoint(_) => None,
        })
        .collect()
}

pub(crate) fn for_prompt(
    journal: &Journal,
    input_modalities: &[InputModality],
) -> Vec<ResponseItem> {
    let mut items = raw_items(journal);
    normalize_history(&mut items, input_modalities);
    items
}

pub(crate) fn estimate_token_count(journal: &Journal, turn_context: &TurnContext) -> Option<i64> {
    let model_info = &turn_context.model_info;
    let personality = turn_context.personality.or(turn_context.config.personality);
    let base_instructions = BaseInstructions {
        text: model_info.get_model_instructions(personality),
    };
    estimate_token_count_with_base_instructions(journal, &base_instructions)
}

pub(crate) fn estimate_token_count_with_base_instructions(
    journal: &Journal,
    base_instructions: &BaseInstructions,
) -> Option<i64> {
    let base_tokens =
        i64::try_from(approx_token_count(&base_instructions.text)).unwrap_or(i64::MAX);

    let items_tokens = raw_items(journal)
        .iter()
        .map(estimate_item_token_count)
        .fold(0i64, i64::saturating_add);

    Some(base_tokens.saturating_add(items_tokens))
}

pub(crate) fn remove_first_item(journal: &mut Journal) -> bool {
    let mut items = raw_items(journal);
    if items.is_empty() {
        return false;
    }

    let removed = items.remove(0);
    thread_history::remove_corresponding_for(&mut items, &removed);
    replace_history(journal, items);
    true
}

pub(crate) fn remove_last_item(journal: &mut Journal) -> bool {
    let mut items = raw_items(journal);
    if let Some(removed) = items.pop() {
        thread_history::remove_corresponding_for(&mut items, &removed);
        replace_history(journal, items);
        true
    } else {
        false
    }
}

pub(crate) fn replace_history(journal: &mut Journal, items: Vec<ResponseItem>) {
    *journal = journal_from_items(items);
}

pub(crate) fn replace_last_turn_images(journal: &mut Journal, placeholder: &str) -> bool {
    let mut items = raw_items(journal);
    let replaced = thread_history::replace_last_turn_images(&mut items, placeholder);
    if replaced {
        replace_history(journal, items);
    }
    replaced
}

pub(crate) fn drop_last_n_user_turns(
    journal: &mut Journal,
    reference_context_item: &mut Option<TurnContextItem>,
    num_turns: u32,
) {
    if num_turns == 0 {
        return;
    }

    let snapshot = raw_items(journal);
    let user_positions = user_turn_boundary_positions(&snapshot);
    let Some(&first_instruction_turn_idx) = user_positions.first() else {
        replace_history(journal, snapshot);
        return;
    };

    let n_from_end = usize::try_from(num_turns).unwrap_or(usize::MAX);
    let mut cut_idx = if n_from_end >= user_positions.len() {
        first_instruction_turn_idx
    } else {
        user_positions[user_positions.len() - n_from_end]
    };

    cut_idx = trim_pre_turn_context_updates(
        reference_context_item,
        &snapshot,
        first_instruction_turn_idx,
        cut_idx,
    );

    replace_history(journal, snapshot[..cut_idx].to_vec());
}

pub(crate) fn update_token_info(
    token_info: &mut Option<TokenUsageInfo>,
    usage: &TokenUsage,
    model_context_window: Option<i64>,
) {
    *token_info =
        TokenUsageInfo::new_or_append(token_info, &Some(usage.clone()), model_context_window);
}

pub(crate) fn set_token_usage_full(token_info: &mut Option<TokenUsageInfo>, context_window: i64) {
    match token_info {
        Some(info) => info.fill_to_context_window(context_window),
        None => {
            *token_info = Some(TokenUsageInfo::full_context_window(context_window));
        }
    }
}

pub(crate) fn get_total_token_usage(
    journal: &Journal,
    token_info: &Option<TokenUsageInfo>,
    server_reasoning_included: bool,
) -> i64 {
    let items = raw_items(journal);
    let last_tokens = token_info
        .as_ref()
        .map(|info| info.last_token_usage.total_tokens)
        .unwrap_or(0);
    let items_after_last_model_generated_tokens = items_after_last_model_generated_item(&items)
        .iter()
        .map(estimate_item_token_count)
        .fold(0i64, i64::saturating_add);
    if server_reasoning_included {
        last_tokens.saturating_add(items_after_last_model_generated_tokens)
    } else {
        last_tokens
            .saturating_add(get_non_last_reasoning_items_tokens(&items))
            .saturating_add(items_after_last_model_generated_tokens)
    }
}

pub(crate) fn get_total_token_usage_breakdown(
    journal: &Journal,
    token_info: &Option<TokenUsageInfo>,
) -> TotalTokenUsageBreakdown {
    let items = raw_items(journal);
    let last_usage = token_info
        .as_ref()
        .map(|info| info.last_token_usage.clone())
        .unwrap_or_default();
    let items_after_last_model_generated = items_after_last_model_generated_item(&items);

    TotalTokenUsageBreakdown {
        last_api_response_total_tokens: last_usage.total_tokens,
        all_history_items_model_visible_bytes: items
            .iter()
            .map(estimate_response_item_model_visible_bytes)
            .fold(0i64, i64::saturating_add),
        estimated_tokens_of_items_added_since_last_successful_api_response:
            items_after_last_model_generated
                .iter()
                .map(estimate_item_token_count)
                .fold(0i64, i64::saturating_add),
        estimated_bytes_of_items_added_since_last_successful_api_response:
            items_after_last_model_generated
                .iter()
                .map(estimate_response_item_model_visible_bytes)
                .fold(0i64, i64::saturating_add),
    }
}

fn push_history_item(journal: &mut Journal, item: ResponseItem) {
    let history_item = JournalTranscriptItem::new(item);
    let key = JournalKey::new(vec!["history".to_string(), history_item.id.clone()]);
    journal.add(key, history_item);
}

fn journal_from_items(items: Vec<ResponseItem>) -> Journal {
    let mut journal = Journal::new();
    for item in items {
        if is_api_message(&item) {
            push_history_item(&mut journal, item);
        }
    }
    journal
}

fn get_non_last_reasoning_items_tokens(items: &[ResponseItem]) -> i64 {
    let Some(last_user_index) = items.iter().rposition(is_user_turn_boundary) else {
        return 0;
    };

    items
        .iter()
        .take(last_user_index)
        .filter(|item| {
            matches!(
                item,
                ResponseItem::Reasoning {
                    encrypted_content: Some(_),
                    ..
                }
            )
        })
        .map(estimate_item_token_count)
        .fold(0i64, i64::saturating_add)
}

fn items_after_last_model_generated_item(items: &[ResponseItem]) -> &[ResponseItem] {
    let start = items
        .iter()
        .rposition(is_model_generated_item)
        .map_or(items.len(), |index| index.saturating_add(1));
    &items[start..]
}

fn normalize_history(items: &mut Vec<ResponseItem>, input_modalities: &[InputModality]) {
    thread_history::ensure_call_outputs_present(items);
    thread_history::remove_orphan_outputs(items);
    thread_history::strip_images_when_unsupported(input_modalities, items);
}

fn trim_pre_turn_context_updates(
    reference_context_item: &mut Option<TurnContextItem>,
    snapshot: &[ResponseItem],
    first_instruction_turn_idx: usize,
    mut cut_idx: usize,
) -> usize {
    while cut_idx > first_instruction_turn_idx {
        match &snapshot[cut_idx - 1] {
            ResponseItem::Message { role, content, .. }
                if role == "developer" && is_contextual_dev_message_content(content) =>
            {
                if has_non_contextual_dev_message_content(content) {
                    *reference_context_item = None;
                }
                cut_idx -= 1;
            }
            ResponseItem::Message { role, content, .. }
                if role == "user" && is_contextual_user_message_content(content) =>
            {
                cut_idx -= 1;
            }
            _ => break,
        }
    }
    cut_idx
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
