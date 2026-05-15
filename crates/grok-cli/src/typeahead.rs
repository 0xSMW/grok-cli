use grok_client::GrokTypeaheadSuggestion;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::time::{Duration, SystemTime};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputTypeaheadSuggestion {
    pub display: String,
    pub insert_text: String,
    pub description: String,
}

impl InputTypeaheadSuggestion {
    pub fn new(display: impl Into<String>) -> Self {
        let display = display.into();
        Self {
            insert_text: display.clone(),
            display,
            description: String::new(),
        }
    }

    pub fn with_insert_text(
        display: impl Into<String>,
        insert_text: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            display: display.into(),
            insert_text: insert_text.into(),
            description: description.into(),
        }
    }
}

#[derive(Clone, Debug)]
struct CacheEntry {
    suggestions: Vec<InputTypeaheadSuggestion>,
    expires_at: SystemTime,
}

#[derive(Clone, Debug)]
pub struct RemoteTypeaheadController {
    min_query_length: usize,
    max_items: usize,
    cache_ttl: Duration,
    cache: HashMap<String, CacheEntry>,
    active_query: Option<String>,
    pending_query: Option<String>,
    latest_query: Option<String>,
    latest_suggestions: Vec<InputTypeaheadSuggestion>,
    latest_version: usize,
}

impl Default for RemoteTypeaheadController {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoteTypeaheadController {
    pub fn new() -> Self {
        Self::with_options(2, 3, Duration::from_secs(60))
    }

    pub fn with_options(min_query_length: usize, max_items: usize, cache_ttl: Duration) -> Self {
        Self {
            min_query_length,
            max_items,
            cache_ttl,
            cache: HashMap::new(),
            active_query: None,
            pending_query: None,
            latest_query: None,
            latest_suggestions: Vec::new(),
            latest_version: 0,
        }
    }

    pub fn version(&self) -> usize {
        self.latest_version
    }

    pub fn suggestions(&self, buffer: &str, now: SystemTime) -> Vec<InputTypeaheadSuggestion> {
        let Some(query) = self.normalized_query(buffer) else {
            return Vec::new();
        };

        if self.latest_query.as_deref() == Some(query.as_str()) {
            return self.latest_suggestions.clone();
        }

        self.display_suggestions(&query, now)
    }

    pub fn is_pending(&self, buffer: &str) -> bool {
        let Some(query) = self.normalized_query(buffer) else {
            return false;
        };
        self.pending_query.as_deref() == Some(query.as_str())
    }

    pub fn observe(&mut self, buffer: &str, now: SystemTime) -> Option<String> {
        let Some(query) = self.normalized_query(buffer) else {
            self.clear();
            return None;
        };

        if self.cached_suggestions(&query, now).is_some() {
            self.active_query = Some(query.clone());
            self.pending_query = None;
            self.update_latest(&query, self.display_suggestions(&query, now));
            return None;
        }

        if self.active_query.as_deref() == Some(query.as_str()) {
            self.update_latest(&query, self.display_suggestions(&query, now));
            return None;
        }

        self.active_query = Some(query.clone());
        self.pending_query = Some(query.clone());
        self.update_latest(&query, self.display_suggestions(&query, now));
        Some(query)
    }

    pub fn store(
        &mut self,
        query: &str,
        suggestions: Vec<InputTypeaheadSuggestion>,
        now: SystemTime,
    ) {
        let suggestions = suggestions
            .into_iter()
            .take(self.max_items)
            .collect::<Vec<_>>();
        self.cache.insert(
            query.to_string(),
            CacheEntry {
                suggestions,
                expires_at: now + self.cache_ttl,
            },
        );

        if self.pending_query.as_deref() == Some(query) {
            self.pending_query = None;
        }

        if self.active_query.as_deref() == Some(query) {
            self.update_latest(query, self.display_suggestions(query, now));
        }
    }

    pub fn reset(&mut self) {
        self.clear();
    }

    pub fn normalize_remote_suggestions(
        suggestions: impl IntoIterator<Item = GrokTypeaheadSuggestion>,
        max_items: usize,
    ) -> Vec<InputTypeaheadSuggestion> {
        suggestions
            .into_iter()
            .filter_map(|suggestion| {
                let text = normalized_suggestion_text(&suggestion.text);
                (!text.is_empty()).then(|| InputTypeaheadSuggestion {
                    display: text.clone(),
                    insert_text: text,
                    description: suggestion.title.unwrap_or_default(),
                })
            })
            .take(max_items)
            .collect()
    }

    fn normalized_query(&self, buffer: &str) -> Option<String> {
        let trimmed = buffer.trim();
        if trimmed.chars().count() < self.min_query_length
            || trimmed.starts_with('/')
            || trimmed.starts_with("[Pasted content ")
            || trimmed.contains('\n')
            || trimmed.contains('\r')
        {
            return None;
        }
        Some(trimmed.to_string())
    }

    fn display_suggestions(&self, query: &str, now: SystemTime) -> Vec<InputTypeaheadSuggestion> {
        if let Some(exact) = self.cached_suggestions(query, now)
            && !exact.is_empty()
        {
            return exact;
        }

        let normalized_query = query.to_lowercase();
        if let Some(latest_query) = &self.latest_query
            && normalized_query.starts_with(&latest_query.to_lowercase())
            && !self.latest_suggestions.is_empty()
        {
            let suggestions = compatible_suggestions(&self.latest_suggestions, &normalized_query);
            if !suggestions.is_empty() {
                return suggestions;
            }
        }

        let mut prefix_keys = self
            .cache
            .iter()
            .filter_map(|(key, entry)| {
                (entry.expires_at > now
                    && !entry.suggestions.is_empty()
                    && normalized_query.starts_with(&key.to_lowercase()))
                .then_some(key)
            })
            .collect::<Vec<_>>();
        prefix_keys.sort_by_key(|key| Reverse(key.len()));

        for key in prefix_keys {
            let suggestions = self
                .cache
                .get(key)
                .map(|entry| compatible_suggestions(&entry.suggestions, &normalized_query))
                .unwrap_or_default();
            if !suggestions.is_empty() {
                return suggestions;
            }
        }

        Vec::new()
    }

    fn cached_suggestions(
        &self,
        query: &str,
        now: SystemTime,
    ) -> Option<Vec<InputTypeaheadSuggestion>> {
        self.cache
            .get(query)
            .filter(|entry| entry.expires_at > now)
            .map(|entry| entry.suggestions.clone())
    }

    fn update_latest(&mut self, query: &str, suggestions: Vec<InputTypeaheadSuggestion>) {
        if self.latest_query.as_deref() == Some(query) && self.latest_suggestions == suggestions {
            return;
        }

        self.latest_query = Some(query.to_string());
        self.latest_suggestions = suggestions;
        self.latest_version += 1;
    }

    fn clear(&mut self) {
        self.active_query = None;
        self.pending_query = None;
        if self.latest_query.is_some() || !self.latest_suggestions.is_empty() {
            self.latest_query = None;
            self.latest_suggestions.clear();
            self.latest_version += 1;
        }
    }
}

fn compatible_suggestions(
    suggestions: &[InputTypeaheadSuggestion],
    normalized_query: &str,
) -> Vec<InputTypeaheadSuggestion> {
    suggestions
        .iter()
        .filter(|suggestion| {
            suggestion
                .insert_text
                .to_lowercase()
                .starts_with(normalized_query)
                || suggestion
                    .display
                    .to_lowercase()
                    .starts_with(normalized_query)
        })
        .cloned()
        .collect()
}

fn normalized_suggestion_text(value: &str) -> String {
    value.replace(['\r', '\n'], " ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::{InputTypeaheadSuggestion, RemoteTypeaheadController};
    use grok_client::GrokTypeaheadSuggestion;
    use serde_json::json;
    use std::time::{Duration, SystemTime};

    fn instant() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000)
    }

    #[test]
    fn remote_typeahead_observes_free_text_only_like_swift() {
        let now = instant();
        let mut controller = RemoteTypeaheadController::new();

        assert_eq!(controller.observe("t", now), None);
        assert_eq!(controller.observe("/model", now), None);
        assert_eq!(controller.observe("[Pasted content 160 chars]", now), None);
        assert_eq!(controller.observe("two\nlines", now), None);

        assert_eq!(controller.observe(" test ", now), Some("test".to_string()));
        assert!(controller.is_pending("test"));
        assert_eq!(controller.version(), 1);
        assert_eq!(controller.suggestions("test", now), Vec::new());
    }

    #[test]
    fn remote_typeahead_stores_exact_cache_and_caps_items_like_swift() {
        let now = instant();
        let mut controller = RemoteTypeaheadController::new();

        assert_eq!(controller.observe("test", now), Some("test".to_string()));
        controller.store(
            "test",
            vec![
                InputTypeaheadSuggestion::new("test driven development"),
                InputTypeaheadSuggestion::new("testing rust"),
                InputTypeaheadSuggestion::new("test fixtures"),
                InputTypeaheadSuggestion::new("test overflow"),
            ],
            now,
        );

        assert!(!controller.is_pending("test"));
        assert_eq!(
            controller
                .suggestions("test", now)
                .into_iter()
                .map(|suggestion| suggestion.display)
                .collect::<Vec<_>>(),
            vec!["test driven development", "testing rust", "test fixtures"]
        );
        assert_eq!(controller.version(), 2);
        assert_eq!(
            controller.observe("test", now + Duration::from_secs(30)),
            None
        );
        assert_eq!(
            controller.observe("toast", now + Duration::from_secs(61)),
            Some("toast".to_string())
        );
        assert_eq!(
            controller.observe("test", now + Duration::from_secs(62)),
            Some("test".to_string())
        );
    }

    #[test]
    fn remote_typeahead_reuses_latest_and_longest_prefix_cache_like_swift() {
        let now = instant();
        let mut controller = RemoteTypeaheadController::new();

        assert_eq!(controller.observe("te", now), Some("te".to_string()));
        controller.store(
            "te",
            vec![
                InputTypeaheadSuggestion::new("test driven development"),
                InputTypeaheadSuggestion::new("team update"),
            ],
            now,
        );

        assert_eq!(
            controller.observe("test", now + Duration::from_secs(1)),
            Some("test".to_string())
        );
        assert_eq!(
            controller
                .suggestions("test", now + Duration::from_secs(1))
                .into_iter()
                .map(|suggestion| suggestion.display)
                .collect::<Vec<_>>(),
            vec!["test driven development"]
        );

        controller.store(
            "tes",
            vec![
                InputTypeaheadSuggestion::new("test fixtures"),
                InputTypeaheadSuggestion::new("test plan"),
            ],
            now + Duration::from_secs(2),
        );
        assert_eq!(
            controller
                .suggestions("test p", now + Duration::from_secs(3))
                .into_iter()
                .map(|suggestion| suggestion.display)
                .collect::<Vec<_>>(),
            vec!["test plan"]
        );
    }

    #[test]
    fn remote_typeahead_normalizes_remote_suggestions_like_swift() {
        let suggestions = RemoteTypeaheadController::normalize_remote_suggestions(
            vec![
                GrokTypeaheadSuggestion {
                    text: "  hello\nworld  ".to_string(),
                    title: Some("Greeting".to_string()),
                    raw_json: json!({"text": "hello"}),
                },
                GrokTypeaheadSuggestion {
                    text: "  \r\n ".to_string(),
                    title: Some("Blank".to_string()),
                    raw_json: json!({"text": ""}),
                },
                GrokTypeaheadSuggestion {
                    text: "third".to_string(),
                    title: None,
                    raw_json: json!({"text": "third"}),
                },
            ],
            3,
        );

        assert_eq!(
            suggestions,
            vec![
                InputTypeaheadSuggestion::with_insert_text(
                    "hello world",
                    "hello world",
                    "Greeting"
                ),
                InputTypeaheadSuggestion::with_insert_text("third", "third", ""),
            ]
        );
    }
}
