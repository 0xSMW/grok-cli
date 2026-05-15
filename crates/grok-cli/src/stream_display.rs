use serde_json::Value;
use std::collections::HashSet;

const HIDDEN_PREAMBLE: &str = "Thinking about your request";
const INTERNAL_TAG_PREFIXES: &[&str] = &["<xai:", "</xai:", "<grok:", "</grok:"];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolActivityKind {
    Thinking,
    Search,
    Tool,
}

impl ToolActivityKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Thinking => "thinking",
            Self::Search => "search",
            Self::Tool => "tool",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolActivityEvent {
    pub kind: ToolActivityKind,
    pub detail: String,
}

impl ToolActivityEvent {
    pub fn display_text(&self) -> String {
        let detail = self.detail.trim();
        if detail.is_empty() {
            self.kind.as_str().to_string()
        } else {
            detail.to_string()
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamDisplayEvent {
    Text(String),
    Activity(ToolActivityEvent),
}

#[derive(Clone, Debug)]
pub struct GrokStreamMarkupParser {
    buffer: String,
    emitted_trace_lines: HashSet<String>,
    hides_hidden_preamble: bool,
}

impl Default for GrokStreamMarkupParser {
    fn default() -> Self {
        Self::new()
    }
}

impl GrokStreamMarkupParser {
    pub fn new() -> Self {
        Self::with_hidden_preamble(true)
    }

    pub fn with_hidden_preamble(hides_hidden_preamble: bool) -> Self {
        Self {
            buffer: String::new(),
            emitted_trace_lines: HashSet::new(),
            hides_hidden_preamble,
        }
    }

    pub fn consume(&mut self, chunk: &str) -> Vec<StreamDisplayEvent> {
        self.buffer.push_str(chunk);
        self.drain(false)
    }

    pub fn finish(&mut self) -> Vec<StreamDisplayEvent> {
        self.drain(true)
    }

    pub fn visible_text(markup: &str, hides_hidden_preamble: bool) -> String {
        let mut parser = Self::with_hidden_preamble(hides_hidden_preamble);
        let events = parser
            .consume(markup)
            .into_iter()
            .chain(parser.finish())
            .collect::<Vec<_>>();
        let text = events
            .into_iter()
            .filter_map(|event| match event {
                StreamDisplayEvent::Text(text) => Some(text),
                StreamDisplayEvent::Activity(_) => None,
            })
            .collect::<String>();
        strip_residual_inline_citation_fragments(&text)
    }

    fn drain(&mut self, final_chunk: bool) -> Vec<StreamDisplayEvent> {
        let mut events = Vec::new();

        while !self.buffer.is_empty() {
            if self.consume_hidden_preamble_if_available(final_chunk) {
                continue;
            }

            if self.hides_hidden_preamble
                && !final_chunk
                && HIDDEN_PREAMBLE.starts_with(self.buffer.as_str())
            {
                break;
            }

            if self.buffer.starts_with("<grok:render") {
                if self.consume_render_directive() {
                    continue;
                }
                if final_chunk {
                    self.buffer.clear();
                }
                break;
            }

            let residual_start = residual_inline_citation_start(&self.buffer);
            let residual_precedes_next_tag = residual_start
                .map(|start| !self.buffer[..start].contains('<'))
                .unwrap_or(false);

            if residual_precedes_next_tag {
                if !final_chunk
                    && residual_start == Some(0)
                    && self.residual_inline_citation_tail_is_incomplete()
                {
                    break;
                }

                if self.consume_residual_inline_citation_fragment(final_chunk, &mut events) {
                    continue;
                }
            }

            let Some(tag_start) = self.buffer.find('<') else {
                if self.hold_incomplete_residual_inline_citation_fragment(final_chunk, &mut events)
                {
                    break;
                }
                let text = std::mem::take(&mut self.buffer);
                self.append_visible_text(&text, &mut events);
                break;
            };

            if tag_start > 0 {
                let text = self.buffer[..tag_start].to_string();
                self.buffer.drain(..tag_start);
                self.append_visible_text(&text, &mut events);
                continue;
            }

            if !final_chunk && is_potential_internal_tag_prefix(&self.buffer) {
                break;
            }

            if self.buffer.starts_with("<xai:tool_usage_card>") {
                if self.consume_tool_usage_card(&mut events) {
                    continue;
                }
                if final_chunk {
                    self.buffer.clear();
                }
                break;
            }

            if starts_with_internal_tag(&self.buffer) {
                if self.consume_internal_tag() {
                    continue;
                }
                if final_chunk {
                    self.buffer.clear();
                }
                break;
            }

            if !final_chunk && self.buffer == "<" {
                break;
            }

            self.append_visible_text("<", &mut events);
            self.buffer.drain(..1);
        }

        events
    }

    fn consume_hidden_preamble_if_available(&mut self, final_chunk: bool) -> bool {
        if !self.hides_hidden_preamble {
            return false;
        }

        if !final_chunk
            && self.buffer.len() < HIDDEN_PREAMBLE.len()
            && HIDDEN_PREAMBLE.starts_with(self.buffer.as_str())
        {
            return false;
        }

        if !self.buffer.starts_with(HIDDEN_PREAMBLE) {
            return false;
        }

        let after_preamble = HIDDEN_PREAMBLE.len();
        if after_preamble < self.buffer.len() {
            let next = self.buffer[after_preamble..].chars().next();
            if !matches!(next, Some('\n' | '\r' | '<')) {
                return false;
            }
        }

        self.buffer.drain(..after_preamble);
        while self.buffer.starts_with(['\n', '\r']) {
            self.buffer.drain(..1);
        }
        true
    }

    fn consume_tool_usage_card(&mut self, events: &mut Vec<StreamDisplayEvent>) -> bool {
        let close_tag = "</xai:tool_usage_card>";
        let Some(close_start) = self.buffer.find(close_tag) else {
            return false;
        };
        let block_end = close_start + close_tag.len();
        let block = self.buffer[..block_end].to_string();
        self.buffer.drain(..block_end);

        let Some(activity) = summarize_tool_usage_card(&block) else {
            return true;
        };
        let trace_line = activity.display_text();
        if self.emitted_trace_lines.insert(trace_line) {
            events.push(StreamDisplayEvent::Activity(activity));
        }
        true
    }

    fn consume_render_directive(&mut self) -> bool {
        let close_tag = "</grok:render>";
        let Some(close_start) = self.buffer.find(close_tag) else {
            return false;
        };
        self.buffer.drain(..close_start + close_tag.len());
        true
    }

    fn consume_internal_tag(&mut self) -> bool {
        let Some(tag_end) = self.buffer.find('>') else {
            return false;
        };
        self.buffer.drain(..=tag_end);
        true
    }

    fn append_visible_text(&self, text: &str, events: &mut Vec<StreamDisplayEvent>) {
        let cleaned = if self.hides_hidden_preamble {
            remove_hidden_preamble_lines(text)
        } else {
            text.to_string()
        };
        let cleaned = strip_residual_inline_citation_fragments(&cleaned);
        if !cleaned.is_empty() {
            events.push(StreamDisplayEvent::Text(cleaned));
        }
    }

    fn hold_incomplete_residual_inline_citation_fragment(
        &mut self,
        final_chunk: bool,
        events: &mut Vec<StreamDisplayEvent>,
    ) -> bool {
        if final_chunk || !self.residual_inline_citation_tail_is_incomplete() {
            return false;
        }

        let Some(start) = residual_inline_citation_start(&self.buffer) else {
            return false;
        };
        if start > 0 {
            let text = self.buffer[..start].to_string();
            self.buffer.drain(..start);
            self.append_visible_text(&text, events);
        }
        true
    }

    fn consume_residual_inline_citation_fragment(
        &mut self,
        final_chunk: bool,
        events: &mut Vec<StreamDisplayEvent>,
    ) -> bool {
        let Some(start) = residual_inline_citation_start(&self.buffer) else {
            return false;
        };

        if start > 0 {
            let text = self.buffer[..start].to_string();
            self.buffer.drain(..start);
            self.append_visible_text(&text, events);
            return true;
        }

        if let Some(close_start) = self.buffer.find("</grok:render>") {
            self.buffer.drain(..close_start + "</grok:render>".len());
            return true;
        }

        if final_chunk {
            self.buffer.clear();
            return true;
        }

        false
    }

    fn residual_inline_citation_tail_is_incomplete(&self) -> bool {
        let Some(start) = residual_inline_citation_start(&self.buffer) else {
            return false;
        };
        let tail = &self.buffer[start..];
        tail.contains("render_inline_citation") && !tail.contains("</grok:render>")
    }
}

fn is_potential_internal_tag_prefix(text: &str) -> bool {
    INTERNAL_TAG_PREFIXES
        .iter()
        .any(|prefix| prefix.starts_with(text))
}

fn starts_with_internal_tag(text: &str) -> bool {
    INTERNAL_TAG_PREFIXES
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

fn residual_inline_citation_start(text: &str) -> Option<usize> {
    let markers = [
        r#"card_type="citation_card""#,
        r#"card_type=\"citation_card\""#,
        r#"type="render_inline_citation""#,
        r#"type=\"render_inline_citation\""#,
    ];
    if !markers.iter().any(|marker| text.contains(marker)) {
        return None;
    }

    [
        r#"card_id="#,
        r#"_id="#,
        r#"card_type="#,
        r#"type="#,
        r#"card_id=\""#,
        r#"_id=\""#,
        r#"card_type=\""#,
        r#"type=\""#,
    ]
    .into_iter()
    .filter_map(|token| text.find(token))
    .min()
}

fn remove_hidden_preamble_lines(text: &str) -> String {
    text.split('\n')
        .filter(|line| line.trim() != HIDDEN_PREAMBLE)
        .collect::<Vec<_>>()
        .join("\n")
}

fn summarize_tool_usage_card(block: &str) -> Option<ToolActivityEvent> {
    let tool_name = xml_value("xai:tool_name", block).unwrap_or_else(|| "tool".to_string());
    let args_text = xml_value("xai:tool_args", block).map(|value| strip_cdata(&value));
    let args = args_text
        .as_ref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok());

    match tool_name.as_str() {
        "web_search" => Some(ToolActivityEvent {
            kind: ToolActivityKind::Search,
            detail: string_value(args.as_ref(), "query")
                .map(|query| compact(&query, 140))
                .unwrap_or_else(|| "searching web".to_string()),
        }),
        "x_search" => Some(ToolActivityEvent {
            kind: ToolActivityKind::Search,
            detail: string_value(args.as_ref(), "query")
                .map(|query| format!("X {}", compact(&query, 140)))
                .unwrap_or_else(|| "searching X".to_string()),
        }),
        "code_execution" | "code" => {
            let code = string_value(args.as_ref(), "code")
                .or(args_text)
                .unwrap_or_default();
            Some(ToolActivityEvent {
                kind: ToolActivityKind::Thinking,
                detail: summarize_code(&code),
            })
        }
        _ => {
            let display_name = title_case_words(&tool_name.replace('_', " "));
            let detail = string_value(args.as_ref(), "query")
                .map(|query| format!("{display_name} {}", compact(&query, 140)))
                .unwrap_or(display_name);
            Some(ToolActivityEvent {
                kind: ToolActivityKind::Tool,
                detail,
            })
        }
    }
}

fn xml_value(tag_name: &str, text: &str) -> Option<String> {
    let open_tag = format!("<{tag_name}>");
    let close_tag = format!("</{tag_name}>");
    let open_start = text.find(&open_tag)?;
    let value_start = open_start + open_tag.len();
    let close_start = text[value_start..].find(&close_tag)? + value_start;
    Some(text[value_start..close_start].to_string())
}

fn strip_cdata(value: &str) -> String {
    let mut stripped = value.trim().to_string();
    if stripped.starts_with("<![CDATA[") {
        stripped.drain(.."<![CDATA[".len());
    }
    if stripped.ends_with("]]>") {
        let end = stripped.len() - "]]>".len();
        stripped.truncate(end);
    }
    stripped
}

fn string_value(value: Option<&Value>, key: &str) -> Option<String> {
    let string = value?.get(key)?.as_str()?;
    (!string.is_empty()).then(|| string.to_string())
}

fn summarize_code(code: &str) -> String {
    for line in code.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            let comment = trimmed.trim_start_matches(['#', ' ']);
            if !comment.is_empty() {
                return compact(comment, 140);
            }
        }
    }

    for line in code.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return compact(trimmed, 140);
        }
    }

    "working through the problem".to_string()
}

fn compact(text: &str, limit: usize) -> String {
    let compacted = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let char_count = compacted.chars().count();
    if char_count <= limit {
        return compacted;
    }

    let prefix = compacted
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    format!("{prefix}...")
}

fn title_case_words(value: &str) -> String {
    value
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            let Some(first) = chars.next() else {
                return String::new();
            };
            format!(
                "{}{}",
                first.to_uppercase().collect::<String>(),
                chars.as_str().to_lowercase()
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_residual_inline_citation_fragments(text: &str) -> String {
    let mut cleaned = strip_tagged_blocks(text, "<grok:render", "</grok:render>");
    while let Some(start) = residual_inline_citation_start(&cleaned) {
        let tail = &cleaned[start..];
        let end = tail
            .find("</grok:render>")
            .map(|index| start + index + "</grok:render>".len())
            .or_else(|| {
                tail.find("</argument>")
                    .map(|index| start + index + "</argument>".len())
            })
            .or_else(|| tail.find('>').map(|index| start + index + 1))
            .unwrap_or(cleaned.len());
        cleaned.drain(start..end);
    }
    strip_tagged_blocks(&cleaned, "<argument", "</argument>")
}

fn strip_tagged_blocks(text: &str, open_prefix: &str, close_tag: &str) -> String {
    let mut cleaned = text.to_string();
    while let Some(start) = cleaned.find(open_prefix) {
        let end = cleaned[start..]
            .find(close_tag)
            .map(|index| start + index + close_tag.len())
            .unwrap_or(cleaned.len());
        cleaned.drain(start..end);
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::{GrokStreamMarkupParser, StreamDisplayEvent, ToolActivityKind};

    fn text(events: Vec<StreamDisplayEvent>) -> String {
        events
            .into_iter()
            .filter_map(|event| match event {
                StreamDisplayEvent::Text(text) => Some(text),
                StreamDisplayEvent::Activity(_) => None,
            })
            .collect()
    }

    #[test]
    fn suppresses_generic_thinking_placeholder_like_swift() {
        let mut parser = GrokStreamMarkupParser::new();
        let events = parser.consume("Thinking about your request\nExample calculation");
        assert_eq!(text(events), "Example calculation");
    }

    #[test]
    fn buffers_split_internal_tags_and_render_directives_like_swift() {
        let mut parser = GrokStreamMarkupParser::new();
        let first = parser.consume("Keep <gro");
        let second = parser.consume(
            r#"k:render type=\"render_inline_citation\"><argument name=\"citation_id\">7</argument></grok:render> done"#,
        );
        let finished = parser.finish();

        assert_eq!(
            text(first.into_iter().chain(second).chain(finished).collect()),
            "Keep  done"
        );
    }

    #[test]
    fn suppresses_split_residual_citation_fragments_like_swift() {
        let mut parser = GrokStreamMarkupParser::new();
        let first = parser.consume(
            r#"Lead _id="ccee26" card_type="citation_card" type="render_inline_citation"><arg"#,
        );
        let second = parser.consume(r#"ument name="citation_id">5</argument></grok:render> tail"#);
        let finished = parser.finish();

        let visible = text(first.into_iter().chain(second).chain(finished).collect());
        assert_eq!(visible, "Lead  tail");
        assert!(!visible.contains("citation_card"));
    }

    #[test]
    fn emits_structured_tool_activity_like_swift() {
        let mut parser = GrokStreamMarkupParser::new();
        let events = parser.consume(
            r#"<xai:tool_usage_card><xai:tool_name>web_search</xai:tool_name><xai:tool_args><![CDATA[{"query":"swift terminal UI"}]]></xai:tool_args></xai:tool_usage_card>"#,
        );

        assert!(events.into_iter().any(|event| {
            matches!(
                event,
                StreamDisplayEvent::Activity(activity)
                    if activity.kind == ToolActivityKind::Search
                        && activity.detail == "swift terminal UI"
            )
        }));
    }
}
