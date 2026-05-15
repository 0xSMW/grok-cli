use crate::picker::PickerItem;

pub fn score(query: &str, text: &str) -> Option<i32> {
    let query = query.trim().to_lowercase();
    let text = text.to_lowercase();
    let text_length = text.chars().count() as i32;

    if query.is_empty() {
        return Some(0);
    }
    if text == query {
        return Some(10_000);
    }
    if text.starts_with(&query) {
        return Some(8_000 - text_length);
    }
    if text.contains(&query) {
        return Some(6_000 - text_length);
    }

    let text_characters = text.chars().collect::<Vec<_>>();
    let mut score = 0;
    let mut search_start = 0;
    let mut previous_match = None;

    for query_character in query.chars() {
        let relative_match = text_characters[search_start..]
            .iter()
            .position(|text_character| *text_character == query_character)?;
        let match_index = search_start + relative_match;

        score += 10;
        if previous_match.is_some_and(|previous_match| previous_match + 1 == match_index) {
            score += 15;
        }
        if match_index == 0
            || matches!(
                text_characters[match_index - 1],
                character if character.is_whitespace()
                    || character == '-'
                    || character == '_'
                    || character == '/'
            )
        {
            score += 8;
        }

        previous_match = Some(match_index);
        search_start = match_index + 1;
    }

    Some(score - text_length)
}

pub fn ranked_picker_items(query: &str, items: &[PickerItem]) -> Vec<PickerItem> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return items.to_vec();
    }

    let mut scored = items
        .iter()
        .filter_map(|item| score(trimmed, &item.search_text).map(|score| (item.clone(), score)))
        .collect::<Vec<_>>();

    scored.sort_by(|(lhs_item, lhs_score), (rhs_item, rhs_score)| {
        rhs_score
            .cmp(lhs_score)
            .then_with(|| lhs_item.title.cmp(&rhs_item.title))
    });

    scored.into_iter().map(|(item, _)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::{ranked_picker_items, score};
    use crate::picker::PickerItem;

    #[test]
    fn fuzzy_matcher_scores_subsequence_matches_like_swift() {
        assert!(score("exp", "Expert expert").is_some());
        assert!(score("wrk", "workspace Research Notes").is_some());
        assert_eq!(score("zzz", "workspace Research Notes"), None);
        assert!(score("/workspace", "/workspace") > score("/wrk", "/workspace"));
    }

    #[test]
    fn fuzzy_matcher_prioritizes_exact_prefix_contains_then_subsequence_like_swift() {
        assert_eq!(score("expert", "expert"), Some(10_000));
        assert!(
            score("exp", "expert").unwrap_or_default() > score("xpr", "expert").unwrap_or_default()
        );
        assert!(
            score("search", "saved search result").unwrap_or_default()
                > score("srh", "saved search result").unwrap_or_default()
        );
    }

    #[test]
    fn fuzzy_matcher_rewards_word_boundaries_and_adjacent_matches_like_swift() {
        assert!(
            score("rn", "Research Notes").unwrap_or_default()
                > score("rn", "arbitrary nonsense").unwrap_or_default()
        );
        assert!(
            score("res", "Research Notes").unwrap_or_default()
                > score("rns", "Research Notes").unwrap_or_default()
        );
    }

    #[test]
    fn fuzzy_matcher_ranks_picker_items_by_score_then_title_like_swift() {
        let mut workspace = PickerItem::new("workspace", "Workspace");
        workspace.search_text = "workspace".to_string();
        let mut search = PickerItem::new("search", "Search");
        search.search_text = "search saved conversations".to_string();
        let mut alpha = PickerItem::new("alpha", "Alpha");
        alpha.search_text = "workspace alpha".to_string();

        let ranked = ranked_picker_items(
            "workspace",
            &[search.clone(), alpha.clone(), workspace.clone()],
        );
        assert_eq!(
            ranked.into_iter().map(|item| item.id).collect::<Vec<_>>(),
            vec!["workspace", "alpha"]
        );

        let mut beta = PickerItem::new("beta", "Beta");
        beta.search_text = "same target".to_string();
        let mut alpha_tie = PickerItem::new("alpha-tie", "Alpha");
        alpha_tie.search_text = "same target".to_string();
        let tied = ranked_picker_items("same", &[beta, alpha_tie]);
        assert_eq!(
            tied.into_iter().map(|item| item.id).collect::<Vec<_>>(),
            vec!["alpha-tie", "beta"]
        );

        let empty = ranked_picker_items("", &[search, workspace, alpha]);
        assert_eq!(
            empty.into_iter().map(|item| item.id).collect::<Vec<_>>(),
            vec!["search", "workspace", "alpha"]
        );
    }
}
