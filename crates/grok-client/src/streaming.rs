use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{GrokError, Result};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchResult {
    pub url: String,
    pub title: String,
    pub preview: String,
    pub site_name: Option<String>,
    pub description: Option<String>,
    pub citation_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XPost {
    pub username: String,
    pub name: String,
    pub text: String,
    pub post_id: String,
    pub create_time: Option<String>,
    pub profile_image_url: Option<String>,
    pub citation_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationResponse {
    pub message: String,
    pub conversation_id: String,
    pub response_id: String,
    #[serde(
        default,
        with = "chrono::serde::ts_seconds_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub timestamp: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search_results: Option<Vec<WebSearchResult>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xposts: Option<Vec<XPost>>,
    #[serde(default)]
    pub is_thinking: bool,
    #[serde(default)]
    pub is_soft_stop: bool,
    #[serde(default)]
    pub is_final: bool,
}

impl ConversationResponse {
    fn token(
        message: String,
        conversation_id: String,
        response_id: String,
        is_thinking: bool,
        is_soft_stop: bool,
    ) -> Self {
        Self {
            message,
            conversation_id,
            response_id,
            timestamp: None,
            web_search_results: None,
            xposts: None,
            is_thinking,
            is_soft_stop,
            is_final: false,
        }
    }

    pub fn final_message(
        message: String,
        conversation_id: String,
        response_id: String,
        web_search_results: Option<Vec<WebSearchResult>>,
        xposts: Option<Vec<XPost>>,
        is_soft_stop: bool,
    ) -> Self {
        Self {
            message,
            conversation_id,
            response_id,
            timestamp: None,
            web_search_results,
            xposts,
            is_thinking: false,
            is_soft_stop,
            is_final: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct StreamingLineReader {
    buffer: Vec<u8>,
    consumed_offset: usize,
    search_offset: usize,
    max_buffered_bytes: usize,
    compaction_threshold: usize,
}

impl Default for StreamingLineReader {
    fn default() -> Self {
        Self::new(1_048_576, 65_536)
    }
}

impl StreamingLineReader {
    pub fn new(max_buffered_bytes: usize, compaction_threshold: usize) -> Self {
        Self {
            buffer: Vec::new(),
            consumed_offset: 0,
            search_offset: 0,
            max_buffered_bytes,
            compaction_threshold,
        }
    }

    pub fn append(&mut self, data: &[u8]) -> Result<Vec<Vec<u8>>> {
        if data.is_empty() {
            return Ok(Vec::new());
        }

        self.buffer.extend_from_slice(data);
        let mut lines = Vec::new();
        while self.search_offset < self.buffer.len() {
            let Some(relative_newline) = self.buffer[self.search_offset..]
                .iter()
                .position(|byte| *byte == b'\n')
            else {
                break;
            };
            let newline_index = self.search_offset + relative_newline;
            lines.push(self.buffer[self.consumed_offset..newline_index].to_vec());
            self.consumed_offset = newline_index + 1;
            self.search_offset = self.consumed_offset;
        }

        self.compact_if_needed();
        self.validate_pending_byte_count()?;
        Ok(lines)
    }

    pub fn append_utf8(&mut self, data: &[u8]) -> Result<Vec<String>> {
        self.append(data)?
            .into_iter()
            .map(line_data_to_string)
            .collect()
    }

    pub fn flush_partial_line(&mut self) -> Option<Vec<u8>> {
        if self.consumed_offset >= self.buffer.len() {
            self.reset();
            return None;
        }

        let line = self.buffer[self.consumed_offset..].to_vec();
        self.reset();
        Some(line)
    }

    pub fn flush_partial_utf8(&mut self) -> Result<Option<String>> {
        self.flush_partial_line()
            .map(line_data_to_string)
            .transpose()
    }

    fn compact_if_needed(&mut self) {
        if self.consumed_offset == 0 {
            return;
        }
        if self.consumed_offset < self.compaction_threshold
            && self.consumed_offset <= self.buffer.len() / 2
        {
            return;
        }

        self.buffer.drain(0..self.consumed_offset);
        self.search_offset -= self.consumed_offset;
        self.consumed_offset = 0;
    }

    fn validate_pending_byte_count(&self) -> Result<()> {
        if self.buffer.len() - self.consumed_offset > self.max_buffered_bytes {
            return Err(GrokError::Streaming(
                "Buffered streaming line exceeded limit".to_string(),
            ));
        }
        Ok(())
    }

    fn reset(&mut self) {
        self.buffer.clear();
        self.consumed_offset = 0;
        self.search_offset = 0;
    }
}

fn line_data_to_string(mut line_data: Vec<u8>) -> Result<String> {
    if line_data.last() == Some(&b'\r') {
        line_data.pop();
    }
    String::from_utf8(line_data).map_err(|error| GrokError::Decoding(error.to_string()))
}

#[derive(Clone, Debug, Default)]
pub struct GrokStreamParser {
    pub conversation_id: String,
    pub response_id: String,
    pub accumulated_message: String,
    pub yielded_final: bool,
    pub finished: bool,
}

impl GrokStreamParser {
    pub fn new(initial_conversation_id: impl Into<String>) -> Self {
        Self {
            conversation_id: initial_conversation_id.into(),
            response_id: String::new(),
            accumulated_message: String::new(),
            yielded_final: false,
            finished: false,
        }
    }

    pub fn consume_line(&mut self, line: &str) -> Result<Option<ConversationResponse>> {
        if self.finished {
            return Ok(None);
        }

        let mut trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }

        if let Some(data) = trimmed.strip_prefix("data:") {
            trimmed = data.trim();
        }

        if trimmed == "[DONE]" {
            self.finished = true;
            return Ok(self.finish());
        }

        let Ok(json) = serde_json::from_str::<Value>(trimmed) else {
            return Ok(None);
        };

        if let Some(error) = json.get("error") {
            return Err(GrokError::Api(describe_api_error(error)));
        }

        let result = json.get("result").and_then(Value::as_object);
        let result_value = result
            .map(|result| Value::Object(result.clone()))
            .unwrap_or_else(|| json.clone());
        let result = result_value.as_object().ok_or_else(|| {
            GrokError::Decoding("Streaming response root was not an object".to_string())
        })?;

        if let Some(error) = result.get("error") {
            return Err(GrokError::Api(describe_api_error(error)));
        }

        let response = dictionary(result.get("response"));
        if let Some(error) = response.and_then(|response| response.get("error")) {
            return Err(GrokError::Api(describe_api_error(error)));
        }

        if let Some(conversation) = dictionary(result.get("conversation"))
            && let Some(id) = string_value(conversation, &["conversationId", "id"])
        {
            self.conversation_id = id;
        }

        let user_response = response
            .and_then(|response| dictionary(response.get("userResponse")))
            .or_else(|| dictionary(result.get("userResponse")));
        let model_response = response
            .and_then(|response| dictionary(response.get("modelResponse")))
            .or_else(|| dictionary(result.get("modelResponse")));

        if let Some(id) = response
            .and_then(|response| string_value(response, &["responseId", "id"]))
            .or_else(|| string_value(result, &["responseId", "id"]))
            .or_else(|| user_response.and_then(|value| string_value(value, &["responseId", "id"])))
            .or_else(|| model_response.and_then(|value| string_value(value, &["responseId", "id"])))
        {
            self.response_id = id;
        }

        let is_soft_stop = response
            .and_then(|response| bool_value(response, &["isSoftStop"]))
            .or_else(|| bool_value(result, &["isSoftStop"]))
            .unwrap_or(false);
        let is_thinking = response
            .and_then(|response| bool_value(response, &["isThinking"]))
            .or_else(|| bool_value(result, &["isThinking"]))
            .unwrap_or(false);

        if let Some(token) = response
            .and_then(|response| string_value_allowing_empty(response, &["token"]))
            .or_else(|| string_value_allowing_empty(result, &["token"]))
        {
            if is_terminal_empty_token(&token, response, result, is_thinking, is_soft_stop) {
                self.yielded_final = true;
                self.finished = true;
                return Ok(Some(ConversationResponse::final_message(
                    self.accumulated_message.clone(),
                    self.conversation_id.clone(),
                    self.response_id.clone(),
                    None,
                    None,
                    is_soft_stop,
                )));
            }

            if !is_thinking {
                self.accumulated_message.push_str(&token);
            }

            return Ok(Some(ConversationResponse::token(
                token,
                self.conversation_id.clone(),
                self.response_id.clone(),
                is_thinking,
                is_soft_stop,
            )));
        }

        if let Some(model_response) = model_response
            && let Some(message) = string_value(model_response, &["message", "text"])
        {
            self.yielded_final = true;
            self.finished = true;
            return Ok(Some(ConversationResponse::final_message(
                message,
                self.conversation_id.clone(),
                self.response_id.clone(),
                extract_web_search_results(model_response),
                extract_xposts(model_response),
                false,
            )));
        }

        Ok(None)
    }

    pub fn finish(&mut self) -> Option<ConversationResponse> {
        if self.yielded_final {
            self.finished = true;
            return None;
        }

        let trimmed = self.accumulated_message.trim().to_string();
        if trimmed.is_empty() {
            self.finished = true;
            return None;
        }

        self.yielded_final = true;
        self.finished = true;
        Some(ConversationResponse::final_message(
            trimmed,
            self.conversation_id.clone(),
            self.response_id.clone(),
            None,
            None,
            false,
        ))
    }
}

fn dictionary(value: Option<&Value>) -> Option<&Map<String, Value>> {
    value.and_then(Value::as_object)
}

fn string_value(dictionary: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        dictionary
            .get(*key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn string_value_allowing_empty(dictionary: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        dictionary
            .get(*key)
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    })
}

fn bool_value(dictionary: &Map<String, Value>, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| dictionary.get(*key).and_then(Value::as_bool))
}

fn contains_any_value(dictionary: Option<&Map<String, Value>>, keys: &[&str]) -> bool {
    dictionary.is_some_and(|dictionary| keys.iter().any(|key| dictionary.contains_key(*key)))
}

fn is_terminal_empty_token(
    token: &str,
    response: Option<&Map<String, Value>>,
    result: &Map<String, Value>,
    is_thinking: bool,
    is_soft_stop: bool,
) -> bool {
    const MARKER_KEYS: &[&str] = &[
        "messageTag",
        "message_tag",
        "messageStepId",
        "message_step_id",
        "toolUsageCardId",
        "tool_usage_card_id",
        "toolUsageCard",
        "toolCallId",
        "tool_call_id",
        "toolName",
        "tool_name",
        "cardId",
        "card_id",
    ];

    token.is_empty()
        && !is_thinking
        && !is_soft_stop
        && !contains_any_value(response, MARKER_KEYS)
        && !contains_any_value(Some(result), MARKER_KEYS)
}

fn extract_web_search_results(model_response: &Map<String, Value>) -> Option<Vec<WebSearchResult>> {
    let results = model_response
        .get("webSearchResults")?
        .as_array()?
        .iter()
        .filter_map(|result| {
            let result = result.as_object()?;
            let url = string_value(result, &["url"])?;
            Some(WebSearchResult {
                title: string_value(result, &["title", "metadataTitle"])
                    .unwrap_or_else(|| url.clone()),
                preview: string_value(result, &["preview", "description", "searchEngineText"])
                    .unwrap_or_default(),
                site_name: string_value(result, &["siteName"]),
                description: string_value(result, &["description"]),
                citation_id: string_value(result, &["citationId"]),
                url,
            })
        })
        .collect::<Vec<_>>();
    (!results.is_empty()).then_some(results)
}

fn extract_xposts(model_response: &Map<String, Value>) -> Option<Vec<XPost>> {
    let posts = model_response
        .get("xposts")?
        .as_array()?
        .iter()
        .filter_map(|post| {
            let post = post.as_object()?;
            let username = string_value(post, &["username"])?;
            Some(XPost {
                name: string_value(post, &["name"]).unwrap_or_else(|| username.clone()),
                text: string_value(post, &["text", "message"]).unwrap_or_default(),
                post_id: string_value(post, &["postId", "id"]).unwrap_or_default(),
                create_time: string_value(post, &["createTime"]),
                profile_image_url: string_value(post, &["profileImageUrl"]),
                citation_id: string_value(post, &["citationId"]),
                username,
            })
        })
        .collect::<Vec<_>>();
    (!posts.is_empty()).then_some(posts)
}

fn describe_api_error(value: &Value) -> String {
    if let Some(message) = value.as_str() {
        return message.to_string();
    }
    if let Some(dictionary) = value.as_object() {
        for key in ["message", "error", "description"] {
            if let Some(message) = dictionary.get(key).and_then(Value::as_str) {
                return message.to_string();
            }
        }
        return Value::Object(dictionary.clone()).to_string();
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::Result;

    use super::{ConversationResponse, GrokStreamParser, StreamingLineReader};

    fn collect_parser(lines: &[&str]) -> Result<Vec<String>> {
        let mut parser = GrokStreamParser::new("");
        let mut messages = Vec::new();
        for line in lines {
            if let Some(response) = parser.consume_line(line)? {
                messages.push(response.message);
                if parser.finished {
                    break;
                }
            }
        }
        if let Some(response) = parser.finish() {
            messages.push(response.message);
        }
        Ok(messages)
    }

    #[test]
    fn streaming_line_reader_handles_arbitrary_chunk_splits_crlf_and_partial_flush() -> Result<()> {
        let mut reader = StreamingLineReader::default();
        let mut lines = Vec::new();
        for chunk in [
            b"fir".as_slice(),
            b"st\r\nsec".as_slice(),
            b"ond\npartial".as_slice(),
        ] {
            lines.extend(reader.append_utf8(chunk)?);
        }
        let Some(partial) = reader.flush_partial_utf8()? else {
            panic!("expected partial line");
        };
        lines.push(partial);

        assert_eq!(lines, ["first", "second", "partial"]);
        Ok(())
    }

    #[test]
    fn streaming_line_reader_rejects_invalid_utf8() {
        let mut reader = StreamingLineReader::default();
        let error = reader.append_utf8(&[0x48, 0x69, 0xff, 0x0a]);

        assert!(
            matches!(error, Err(error) if error.to_string().contains("Could not decode Grok response"))
        );
    }

    #[test]
    fn stream_parser_yields_tokens_and_final_model_response() -> Result<()> {
        let messages = collect_parser(&[
            r#"{"result":{"conversation":{"conversationId":"convo123"},"response":{"responseId":"resp777","token":"Hello "}}}"#,
            r#"{"result":{"response":{"responseId":"resp777","token":"World"}}}"#,
            r#"{"result":{"response":{"modelResponse":{"message":"Hello World","responseId":"resp777"}}}}"#,
        ])?;

        assert_eq!(messages, ["Hello ", "World", "Hello World"]);
        Ok(())
    }

    #[test]
    fn conversation_response_decodes_timestamp_sources_and_default_flags_like_swift() -> Result<()>
    {
        let response: ConversationResponse = serde_json::from_value(json!({
            "message": "Hello from Grok",
            "conversationId": "convo123",
            "responseId": "resp456",
            "timestamp": 1_742_045_040,
            "webSearchResults": [
                {
                    "url": "https://example.com",
                    "title": "Example Site",
                    "preview": "Preview text..."
                }
            ],
            "xposts": [
                {
                    "username": "testUser",
                    "name": "Test Name",
                    "text": "Post content here",
                    "postId": "xyz789"
                }
            ]
        }))?;

        assert_eq!(response.conversation_id, "convo123");
        assert_eq!(response.response_id, "resp456");
        assert_eq!(
            response.timestamp.map(|timestamp| timestamp.timestamp()),
            Some(1_742_045_040)
        );
        assert_eq!(response.web_search_results.unwrap_or_default().len(), 1);
        assert_eq!(response.xposts.unwrap_or_default().len(), 1);
        assert!(!response.is_thinking);
        assert!(!response.is_soft_stop);
        assert!(!response.is_final);
        Ok(())
    }

    #[test]
    fn conversation_response_decodes_unix_timestamp_seconds_exactly() -> Result<()> {
        let response: ConversationResponse = serde_json::from_value(json!({
            "message": "Hello from Grok",
            "conversationId": "convo123",
            "responseId": "resp456",
            "timestamp": 1_742_045_040
        }))?;

        let Some(timestamp) = response.timestamp else {
            panic!("timestamp should decode");
        };
        assert_eq!(timestamp.timestamp(), 1_742_045_040);
        assert_eq!(timestamp.timestamp_subsec_nanos(), 0);
        assert_eq!(timestamp.to_rfc3339(), "2025-03-15T13:24:00+00:00");
        Ok(())
    }

    #[test]
    fn stream_parser_extracts_sources_and_filters_empty_entries_like_swift() -> Result<()> {
        let mut parser = GrokStreamParser::new("");
        let response = parser
            .consume_line(
                r#"{"result":{"conversation":{"conversationId":"convo123"},"response":{"modelResponse":{"message":"Answer","responseId":"resp123","webSearchResults":[{"url":"https://example.com","title":"Example","preview":"Preview","siteName":"Site","description":"Desc","citationId":"cit1"},{"url":"","title":"Ignore me","preview":"Preview","description":"Desc"}],"xposts":[{"username":"stephen","name":"Stephen","text":"Hello from X","createTime":"2025-03-15T08:00:00Z","profileImageUrl":"","postId":"post123","citationId":""},{"username":"","name":"Jane","text":"No username","postId":"post456"}]}}}}"#,
            )?
            .unwrap_or_else(|| panic!("expected final response"));

        let web_results = response.web_search_results.unwrap_or_default();
        assert_eq!(web_results.len(), 1);
        assert_eq!(web_results[0].url, "https://example.com");
        assert_eq!(web_results[0].title, "Example");
        assert_eq!(web_results[0].site_name.as_deref(), Some("Site"));
        assert_eq!(web_results[0].description.as_deref(), Some("Desc"));
        assert_eq!(web_results[0].citation_id.as_deref(), Some("cit1"));

        let posts = response.xposts.unwrap_or_default();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].username, "stephen");
        assert_eq!(posts[0].post_id, "post123");
        assert_eq!(
            posts[0].create_time.as_deref(),
            Some("2025-03-15T08:00:00Z")
        );
        assert_eq!(posts[0].profile_image_url, None);
        assert_eq!(posts[0].citation_id, None);
        Ok(())
    }

    #[test]
    fn stream_parser_treats_plain_empty_token_as_terminal() -> Result<()> {
        let messages = collect_parser(&[
            r#"{"result":{"conversation":{"conversationId":"convo123"},"response":{"responseId":"resp777","token":"Hello "}}}"#,
            r#"{"result":{"response":{"responseId":"resp777","token":"World"}}}"#,
            r#"{"result":{"response":{"responseId":"resp777","token":"","isThinking":false,"isSoftStop":false}}}"#,
            r#"{"result":{"response":{"modelResponse":{"message":"Ignored delayed final","responseId":"resp777"}}}}"#,
        ])?;

        assert_eq!(messages, ["Hello ", "World", "Hello World"]);
        Ok(())
    }

    #[test]
    fn stream_parser_does_not_treat_tool_empty_token_as_terminal() -> Result<()> {
        let messages = collect_parser(&[
            r#"{"result":{"conversation":{"conversationId":"convo123"},"response":{"responseId":"resp777","token":"Hello "}}}"#,
            r#"{"result":{"responseId":"resp777","token":"","isThinking":false,"isSoftStop":false,"messageTag":"raw_function_result","messageStepId":0,"toolUsageCardId":"tool-1"}}"#,
            r#"{"result":{"response":{"responseId":"resp777","token":"World"}}}"#,
            r#"{"result":{"response":{"modelResponse":{"message":"Hello World","responseId":"resp777"}}}}"#,
        ])?;

        assert_eq!(messages, ["Hello ", "", "World", "Hello World"]);
        Ok(())
    }

    #[test]
    fn stream_parser_keeps_thinking_tokens_out_of_fallback_final() -> Result<()> {
        let messages = collect_parser(&[
            r#"{"result":{"conversation":{"conversationId":"convo123"},"response":{"responseId":"resp777","token":"long silk"}}}"#,
            r#"{"result":{"response":{"responseId":"resp777","token":"Responding as a beautiful Asian woman","isThinking":true}}}"#,
            r#"{"result":{"response":{"responseId":"resp777","token":"y black hair"}}}"#,
        ])?;

        assert_eq!(
            messages,
            [
                "long silk",
                "Responding as a beautiful Asian woman",
                "y black hair",
                "long silky black hair"
            ]
        );
        Ok(())
    }

    #[test]
    fn stream_parser_treats_done_as_terminal_signal() -> Result<()> {
        let messages = collect_parser(&[
            r#"data: {"result":{"conversation":{"conversationId":"convo-done"},"response":{"responseId":"resp-done","token":"Hello"}}}"#,
            "data: [DONE]",
            r#"data: {"result":{"response":{"responseId":"resp-done","token":" ignored"}}}"#,
        ])?;

        assert_eq!(messages, ["Hello", "Hello"]);
        Ok(())
    }
}
