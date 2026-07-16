//! Tests for the shared [`tui_prompt_history`] helper used by the TUI
//! up-arrow prompt-history menu. These guard the GUI-parity ordering, dedupe,
//! prefix filtering, and whitespace exclusion (PRODUCT.md invariants 8, 9, 11,
//! 13) against drift.
use warpui::{App, EntityId};

use super::tui_prompt_history;
use crate::ai::blocklist::BlocklistAIHistoryModel;
use crate::suggestions::ignored_suggestions_model::{IgnoredSuggestionsModel, SuggestionType};

/// Asserts that querying a history seeded with `prompts` (oldest-first) yields
/// exactly `expected`.
fn assert_prompt_history(prompts: &[&str], query: &str, expected: &[&str]) {
    let prompts: Vec<String> = prompts.iter().map(|prompt| (*prompt).to_owned()).collect();
    let query = query.to_owned();
    let expected: Vec<String> = expected.iter().map(|entry| (*entry).to_owned()).collect();
    App::test((), |app| async move {
        let terminal_surface_id = EntityId::new();
        app.add_singleton_model(move |_| BlocklistAIHistoryModel::mock_with_ai_queries(prompts));
        app.read(|ctx| {
            let texts: Vec<String> = tui_prompt_history(terminal_surface_id, &query, ctx)
                .into_iter()
                .map(|entry| entry.query_text)
                .collect();
            assert_eq!(texts, expected);
        });
    });
}

#[test]
fn tui_prompt_history_dedupes_orders_and_excludes_whitespace() {
    // Oldest-first submission order. "deploy the app" appears twice; the newer
    // occurrence wins and the older is dropped. The whitespace-only prompt must
    // never appear.
    assert_prompt_history(
        &[
            "deploy the app",
            "delete the cache",
            "deploy the app",
            "   ",
            "build the project",
        ],
        "",
        &["delete the cache", "deploy the app", "build the project"],
    );
}

#[test]
fn tui_prompt_history_prefix_filters_by_trimmed_query() {
    let prompts = &["deploy the app", "delete the cache", "build the project"];
    assert_prompt_history(prompts, "de", &["deploy the app", "delete the cache"]);
    // Leading/trailing whitespace in the query is trimmed before matching.
    assert_prompt_history(prompts, "  deploy ", &["deploy the app"]);
    assert_prompt_history(prompts, "xyz", &[]);
}

#[test]
fn tui_prompt_history_excludes_ignored_prompts() {
    let prompts: Vec<String> = ["deploy the app", "delete the cache", "build the project"]
        .iter()
        .map(|prompt| (*prompt).to_owned())
        .collect();
    App::test((), |app| async move {
        let terminal_surface_id = EntityId::new();
        app.add_singleton_model(move |_| BlocklistAIHistoryModel::mock_with_ai_queries(prompts));
        app.add_singleton_model(|_| {
            IgnoredSuggestionsModel::new(vec![(
                "delete the cache".to_owned(),
                SuggestionType::AIQuery,
            )])
        });
        app.read(|ctx| {
            let texts: Vec<String> = tui_prompt_history(terminal_surface_id, "", ctx)
                .into_iter()
                .map(|entry| entry.query_text)
                .collect();
            // The ignored prompt is excluded; the rest remain in order.
            assert_eq!(
                texts,
                vec!["deploy the app".to_owned(), "build the project".to_owned()]
            );
        });
    });
}
