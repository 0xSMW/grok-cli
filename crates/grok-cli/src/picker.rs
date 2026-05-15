use crate::input::wrapped_line_count;
use crate::terminal::{truncate_end, visible_length};
use std::collections::HashSet;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArrowSelection<T> {
    Selected(T),
    Cancelled,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PickerItem {
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub metadata_label: Option<String>,
    pub metadata: Option<String>,
    pub preview_label: String,
    pub preview: Option<String>,
    pub is_enabled: bool,
    pub search_text: String,
}

impl PickerItem {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        let id = id.into();
        let title = title.into();
        Self {
            search_text: [title.as_str(), id.as_str()].join(" "),
            id,
            title,
            subtitle: None,
            metadata_label: None,
            metadata: None,
            preview_label: "preview".to_string(),
            preview: None,
            is_enabled: true,
        }
    }

    pub fn with_search_text(mut self, search_text: impl Into<String>) -> Self {
        self.search_text = search_text.into();
        self
    }

    pub fn rebuild_search_text(&mut self) {
        self.search_text = [
            Some(self.title.as_str()),
            self.subtitle.as_deref(),
            self.metadata.as_deref(),
            self.preview.as_deref(),
            Some(self.id.as_str()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PickerLineOptions {
    pub selected_preview_override: Option<String>,
    pub is_loading: bool,
    pub error_message: Option<String>,
}

pub fn visible_window_start(
    item_count: usize,
    selected_index: isize,
    visible_limit: usize,
) -> usize {
    if item_count == 0 || visible_limit == 0 {
        return 0;
    }

    let clamped_selected_index =
        selected_index.clamp(0, item_count.saturating_sub(1) as isize) as usize;
    let max_start = item_count.saturating_sub(visible_limit);
    if clamped_selected_index < visible_limit {
        return 0;
    }
    (clamped_selected_index - visible_limit + 1).min(max_start)
}

pub fn resolve_selection<T, F>(arrow_selection: ArrowSelection<T>, fallback: F) -> Option<T>
where
    F: FnOnce() -> Option<T>,
{
    match arrow_selection {
        ArrowSelection::Selected(value) => Some(value),
        ArrowSelection::Cancelled => None,
        ArrowSelection::Unavailable => fallback(),
    }
}

pub fn visible_items(
    query: &str,
    items: &[PickerItem],
    uses_remote_items: bool,
) -> Vec<PickerItem> {
    if uses_remote_items {
        items.to_vec()
    } else {
        crate::fuzzy::ranked_picker_items(query, items)
    }
}

pub fn initial_index(items: &[PickerItem], current_id: Option<&str>) -> usize {
    if let Some(current_id) = current_id
        && let Some(index) = items
            .iter()
            .position(|item| item.id == current_id && item.is_enabled)
    {
        return index;
    }
    items
        .iter()
        .position(|item| item.is_enabled)
        .unwrap_or_default()
}

pub fn next_index(current_index: usize, delta: isize, items: &[PickerItem]) -> usize {
    if items.is_empty() {
        return current_index;
    }

    let mut candidate = current_index.min(items.len().saturating_sub(1));
    for _ in 0..items.len() {
        candidate = ((candidate as isize + delta).rem_euclid(items.len() as isize)) as usize;
        if items[candidate].is_enabled {
            return candidate;
        }
    }
    current_index.min(items.len().saturating_sub(1))
}

pub fn next_page_index(current_index: usize, delta: isize, items: &[PickerItem]) -> usize {
    if items.is_empty() {
        return current_index;
    }

    let current_index = current_index.min(items.len().saturating_sub(1));
    let target =
        (current_index as isize + delta).clamp(0, items.len().saturating_sub(1) as isize) as usize;
    if items[target].is_enabled {
        return target;
    }

    let step = if delta < 0 { -1 } else { 1 };
    let mut candidate = target as isize;
    while (0..items.len() as isize).contains(&candidate) {
        if items[candidate as usize].is_enabled {
            return candidate as usize;
        }
        candidate += step;
    }
    current_index
}

pub fn lines(
    title: &str,
    query: &str,
    items: &[PickerItem],
    selected_index: isize,
    width: usize,
    options: &PickerLineOptions,
) -> Vec<String> {
    let query_line = if options.is_loading {
        format!("query {query}  searching...")
    } else {
        format!("query {query}")
    };
    let mut lines = vec![bold(&cyan(title)), blue(&query_line)];
    let title_width = title_column_width(items, width);

    if items.is_empty() {
        lines.push(light_black(
            options
                .error_message
                .as_deref()
                .unwrap_or("No conversations found."),
        ));
    } else {
        for (index, item) in items.iter().enumerate() {
            let is_selected = index as isize == selected_index;
            let marker = if is_selected { "> " } else { "  " };
            let rendered_title = padded_end(&truncate_end(&item.title, title_width), title_width);
            let subtitle = item.subtitle.as_deref().unwrap_or(&item.id);
            let subtitle_width = (width as isize
                - visible_length(marker) as isize
                - title_width as isize
                - 1)
            .max(0) as usize;
            let suffix = if subtitle_width > 4 {
                format!(" {}", truncate_end(subtitle, subtitle_width - 1))
            } else {
                String::new()
            };
            let base = format!("{marker}{rendered_title}{suffix}");
            lines.push(if item.is_enabled {
                if is_selected {
                    bold(&yellow(&base))
                } else {
                    yellow(&base)
                }
            } else {
                light_black(&base)
            });
        }
    }

    if let Some(item) = selected_item(items, selected_index)
        && let Some(metadata) = item.metadata.as_deref()
        && !metadata.is_empty()
    {
        lines.push(String::new());
        lines.push(cyan(item.metadata_label.as_deref().unwrap_or("metadata")));
        lines.push(metadata.to_string());
    }

    if let Some(item) = selected_item(items, selected_index) {
        let selected_preview = options
            .selected_preview_override
            .as_deref()
            .or(item.preview.as_deref());
        if let Some(selected_preview) = selected_preview
            && !selected_preview.is_empty()
        {
            lines.push(String::new());
            lines.push(cyan(&item.preview_label));
            lines.push(selected_preview.to_string());
        }
    }

    lines.push(String::new());
    lines.push(blue("enter choose | type filter | esc cancel"));
    lines
}

pub fn preview_prefetch_items(
    items: &[PickerItem],
    selected_index: isize,
    previous_selected_index: Option<isize>,
    visible_limit: usize,
    directional_lookahead: usize,
    opposite_lookahead: usize,
) -> Vec<PickerItem> {
    if items.is_empty() {
        return Vec::new();
    }

    let selected_index = selected_index.clamp(0, items.len().saturating_sub(1) as isize);
    let direction = if previous_selected_index.is_some_and(|previous| previous > selected_index) {
        -1
    } else {
        1
    };

    let mut indexes = vec![selected_index];
    for offset in 1..=directional_lookahead {
        indexes.push(selected_index + direction * offset as isize);
    }

    let window_start = visible_window_start(items.len(), selected_index, visible_limit);
    indexes.extend(
        (window_start..(window_start + visible_limit).min(items.len())).map(|index| index as isize),
    );

    for offset in 1..=opposite_lookahead {
        indexes.push(selected_index - direction * offset as isize);
    }

    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for index in indexes {
        if index < 0 {
            continue;
        }
        let Some(item) = items.get(index as usize) else {
            continue;
        };
        if item.is_enabled && seen.insert(item.id.clone()) {
            result.push(item.clone());
        }
    }
    result
}

pub fn terminal_row_count(lines: &[String], width: usize) -> usize {
    lines
        .iter()
        .map(|line| terminal_row_count_for_line(line, width))
        .sum()
}

fn terminal_row_count_for_line(line: &str, width: usize) -> usize {
    line.split('\n')
        .map(|segment| wrapped_line_count(visible_length(segment), width))
        .sum()
}

fn title_column_width(items: &[PickerItem], width: usize) -> usize {
    let max_title_width = items
        .iter()
        .map(|item| visible_length(&item.title))
        .max()
        .unwrap_or(0);
    let max_subtitle_width = items
        .iter()
        .map(|item| visible_length(item.subtitle.as_deref().unwrap_or(&item.id)))
        .max()
        .unwrap_or(0);
    let subtitle_budget = max_subtitle_width.min(24);
    let available = ((width as isize)
        - 3
        - if subtitle_budget > 0 {
            subtitle_budget as isize + 1
        } else {
            0
        })
    .max(24) as usize;
    max_title_width.min(available).max(12)
}

fn padded_end(value: &str, width: usize) -> String {
    let value_length = visible_length(value);
    if value_length >= width {
        return value.to_string();
    }
    format!("{value}{}", " ".repeat(width - value_length))
}

fn selected_item(items: &[PickerItem], selected_index: isize) -> Option<&PickerItem> {
    if selected_index < 0 {
        return None;
    }
    items.get(selected_index as usize)
}

fn cyan(value: &str) -> String {
    format!("\u{001b}[36m{value}\u{001b}[0m")
}

fn blue(value: &str) -> String {
    format!("\u{001b}[34m{value}\u{001b}[0m")
}

fn yellow(value: &str) -> String {
    format!("\u{001b}[33m{value}\u{001b}[0m")
}

fn light_black(value: &str) -> String {
    format!("\u{001b}[90m{value}\u{001b}[0m")
}

fn bold(value: &str) -> String {
    format!("\u{001b}[1m{value}\u{001b}[22m")
}

#[cfg(test)]
mod tests {
    use super::{
        ArrowSelection, PickerItem, PickerLineOptions, initial_index, lines, next_index,
        next_page_index, preview_prefetch_items, resolve_selection, terminal_row_count,
        visible_items, visible_window_start,
    };
    use crate::terminal::strip_ansi;

    #[test]
    fn interactive_picker_does_not_hard_truncate_titles_at_twenty_two_characters_like_swift() {
        let title = "Lost Gospel: Jesus Babe Magnet";
        let item = PickerItem {
            subtitle: Some("recent".to_string()),
            ..PickerItem::new("conv-1", title)
        };

        let rendered = strip_ansi(
            &lines(
                "Select conversation",
                "",
                &[item],
                0,
                80,
                &PickerLineOptions::default(),
            )
            .join("\n"),
        );

        assert!(rendered.contains(title));
        assert!(!rendered.contains("Lost Gospel: Jesus Bab "));
    }

    #[test]
    fn interactive_picker_labels_date_metadata_separately_from_preview_like_swift() {
        let item = PickerItem {
            subtitle: Some("modified 2026-05-13T00:00:00Z".to_string()),
            metadata_label: Some("modified".to_string()),
            metadata: Some("2026-05-13T00:00:00Z".to_string()),
            preview: Some("Actual last message\nSecond preview line".to_string()),
            ..PickerItem::new("conv-1", "Mock Conversation")
        };

        let rendered = strip_ansi(
            &lines(
                "Select conversation",
                "",
                &[item],
                0,
                100,
                &PickerLineOptions::default(),
            )
            .join("\n"),
        );

        assert!(rendered.contains("\nmodified\n2026-05-13T00:00:00Z\n"));
        assert!(rendered.contains("\npreview\nActual last message\nSecond preview line\n"));
        assert!(!rendered.contains("\npreview\n2026-05-13T00:00:00Z"));
    }

    #[test]
    fn interactive_picker_counts_wrapped_and_multiline_preview_rows_like_swift() {
        let rows = terminal_row_count(
            &[
                "Select conversation".to_string(),
                "preview".to_string(),
                "first preview line\nsecond preview line".to_string(),
                "x".repeat(25),
            ],
            10,
        );

        assert_eq!(rows, 10);
    }

    #[test]
    fn interactive_picker_prefetches_selected_visible_and_directional_lookahead_first_like_swift() {
        let items = (0..20)
            .map(|index| PickerItem::new(format!("conv-{index}"), format!("Conversation {index}")))
            .collect::<Vec<_>>();

        let down = preview_prefetch_items(&items, 8, Some(7), 8, 3, 1)
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>();

        assert_eq!(
            down.iter().take(5).cloned().collect::<Vec<_>>(),
            vec!["conv-8", "conv-9", "conv-10", "conv-11", "conv-1"]
        );
        let down_conv_9 = down
            .iter()
            .position(|id| id == "conv-9")
            .unwrap_or(usize::MAX);
        let down_conv_7 = down
            .iter()
            .position(|id| id == "conv-7")
            .unwrap_or(usize::MAX);
        assert!(down_conv_9 < down_conv_7);

        let up = preview_prefetch_items(&items, 8, Some(9), 8, 3, 1)
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>();
        let up_conv_7 = up
            .iter()
            .position(|id| id == "conv-7")
            .unwrap_or(usize::MAX);
        let up_conv_9 = up
            .iter()
            .position(|id| id == "conv-9")
            .unwrap_or(usize::MAX);
        assert!(up_conv_7 < up_conv_9);
    }

    #[test]
    fn interactive_picker_scroll_window_follows_selection_past_initial_items_like_swift() {
        assert_eq!(visible_window_start(50, 0, DEFAULT_VISIBLE_LIMIT), 0);
        assert_eq!(visible_window_start(50, 7, DEFAULT_VISIBLE_LIMIT), 0);
        assert_eq!(visible_window_start(50, 8, DEFAULT_VISIBLE_LIMIT), 1);
        assert_eq!(visible_window_start(50, 49, DEFAULT_VISIBLE_LIMIT), 42);
    }

    #[test]
    fn interactive_picker_cancel_does_not_fall_back_to_numbered_selection_like_swift() {
        let mut fallback_was_called = false;
        let selection = resolve_selection(ArrowSelection::<String>::Cancelled, || {
            fallback_was_called = true;
            Some("fallback".to_string())
        });

        assert_eq!(selection, None);
        assert!(!fallback_was_called);

        let fallback = resolve_selection(ArrowSelection::Unavailable, || Some("fallback"));
        assert_eq!(fallback, Some("fallback"));
        let selected = resolve_selection(ArrowSelection::Selected("chosen"), || Some("fallback"));
        assert_eq!(selected, Some("chosen"));
    }

    #[test]
    fn interactive_picker_navigation_skips_disabled_items_like_swift() {
        let items = vec![
            PickerItem {
                is_enabled: false,
                ..PickerItem::new("disabled-first", "Disabled First")
            },
            PickerItem::new("enabled-one", "Enabled One"),
            PickerItem {
                is_enabled: false,
                ..PickerItem::new("disabled-two", "Disabled Two")
            },
            PickerItem::new("enabled-two", "Enabled Two"),
        ];

        assert_eq!(initial_index(&items, None), 1);
        assert_eq!(initial_index(&items, Some("enabled-two")), 3);
        assert_eq!(initial_index(&items, Some("disabled-first")), 1);
        assert_eq!(next_index(1, 1, &items), 3);
        assert_eq!(next_index(3, 1, &items), 1);
        assert_eq!(next_index(1, -1, &items), 3);
        assert_eq!(next_page_index(1, 2, &items), 3);
        assert_eq!(next_page_index(3, -2, &items), 1);
    }

    #[test]
    fn interactive_picker_visible_items_only_ranks_local_results_like_swift() {
        let mut search = PickerItem::new("search", "Search");
        search.search_text = "saved conversations".to_string();
        let mut workspace = PickerItem::new("workspace", "Workspace");
        workspace.search_text = "workspace research notes".to_string();

        let local = visible_items("workspace", &[search.clone(), workspace.clone()], false)
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>();
        assert_eq!(local, vec!["workspace"]);

        let remote = visible_items("workspace", &[search, workspace], true)
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>();
        assert_eq!(remote, vec!["search", "workspace"]);
    }

    const DEFAULT_VISIBLE_LIMIT: usize = 8;
}
