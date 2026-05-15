use serde_json::{Map, Value};

use crate::{GrokClient, GrokConversationListOptions, JsonLookup, Result};

#[derive(Clone, Debug, PartialEq)]
pub struct GrokConversation {
    pub conversation_id: String,
    pub title: String,
    pub starred: bool,
    pub create_time: String,
    pub modify_time: String,
    pub system_prompt_name: String,
    pub temporary: bool,
    pub media_types: Vec<String>,
    pub preview: String,
    pub raw_json: Value,
}

impl GrokConversation {
    fn from_dictionary(dictionary: Map<String, Value>) -> Option<Self> {
        let lookup = JsonLookup::new(Value::Object(dictionary.clone()));
        let conversation_id = lookup.string(&["conversationId", "conversation_id", "id"])?;
        let title = lookup
            .string(&["title", "name"])
            .unwrap_or_else(|| conversation_id.clone());
        let media_types = lookup
            .raw()
            .as_object()
            .and_then(|object| {
                object
                    .get("mediaTypes")
                    .or_else(|| object.get("media_types"))
            })
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Some(Self {
            conversation_id,
            title,
            starred: lookup.bool(&["starred"]).unwrap_or(false),
            create_time: lookup
                .string_allowing_empty(&["createTime", "create_time"])
                .unwrap_or_default(),
            modify_time: lookup
                .string_allowing_empty(&["modifyTime", "modify_time"])
                .unwrap_or_default(),
            system_prompt_name: lookup
                .string_allowing_empty(&["systemPromptName", "system_prompt_name"])
                .unwrap_or_default(),
            temporary: lookup.bool(&["temporary"]).unwrap_or(false),
            media_types,
            preview: lookup
                .string_allowing_empty(&[
                    "preview",
                    "snippet",
                    "lastMessage",
                    "last_message",
                    "lastResponse",
                    "last_response",
                    "description",
                ])
                .unwrap_or_default(),
            raw_json: Value::Object(dictionary),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokConversationsResponse {
    pub conversations: Vec<GrokConversation>,
    pub next_page_token: Option<String>,
    pub text_search_matches: Vec<String>,
    pub raw_json: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokConversationV2Response {
    pub conversation_id: Option<String>,
    pub raw_json: Value,
}

impl GrokConversationV2Response {
    pub fn from_raw_json(raw_json: Value) -> Self {
        let conversation = JsonLookup::new(raw_json.clone())
            .first_dictionary(&["conversation", "data", "result"])
            .map(Value::Object)
            .unwrap_or_else(|| raw_json.clone());
        let conversation_id =
            conversation_v2_id(&conversation, &["conversationId", "conversation_id", "id"]);

        Self {
            conversation_id,
            raw_json,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrokResponseNode {
    pub response_id: String,
    pub sender: String,
    pub parent_response_id: Option<String>,
}

impl GrokResponseNode {
    fn from_dictionary(dictionary: Map<String, Value>) -> Option<Self> {
        let lookup = JsonLookup::new(Value::Object(dictionary));
        Some(Self {
            response_id: lookup.string(&["responseId", "id"])?,
            sender: lookup.string(&["sender"])?,
            parent_response_id: lookup.string(&["parentResponseId", "parent_response_id"]),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrokConversationMessage {
    pub response_id: String,
    pub message: String,
    pub sender: String,
    pub create_time: String,
    pub parent_response_id: Option<String>,
    pub mode_id: Option<String>,
    pub model_id: Option<String>,
    pub mode_name: Option<String>,
    pub model_name: Option<String>,
}

impl GrokConversationMessage {
    fn from_dictionary(dictionary: Map<String, Value>) -> Option<Self> {
        let lookup = JsonLookup::new(Value::Object(dictionary.clone()));
        let metadata = nested_dictionary(&dictionary, &["metadata"]);
        let request_metadata = metadata.as_ref().and_then(|metadata| {
            nested_dictionary(metadata, &["request_metadata", "requestMetadata"])
        });
        let mode = nested_dictionary(&dictionary, &["mode"]);
        let model = nested_dictionary(&dictionary, &["model"]);
        let mode_scalar = lookup.string(&["mode"]);
        let model_scalar = lookup.string(&["model"]);

        Some(Self {
            response_id: lookup.string(&["responseId"])?,
            message: lookup.string_allowing_empty(&["message"])?,
            sender: lookup.string(&["sender"])?,
            create_time: lookup.string_allowing_empty(&["createTime"])?,
            parent_response_id: lookup.string(&["parentResponseId", "parent_response_id"]),
            mode_id: lookup
                .string(&["modeId", "mode_id"])
                .or(mode_scalar.clone())
                .or_else(|| {
                    first_nested_string(
                        &[mode.as_ref(), request_metadata.as_ref(), metadata.as_ref()],
                        &["modeId", "mode_id", "mode", "id", "value"],
                    )
                }),
            model_id: lookup
                .string(&["modelId", "model_id"])
                .or(model_scalar)
                .or_else(|| {
                    first_nested_string(
                        &[model.as_ref(), request_metadata.as_ref(), metadata.as_ref()],
                        &["modelId", "model_id", "model", "id", "value"],
                    )
                }),
            mode_name: lookup.string(&["modeName", "mode_name"]).or_else(|| {
                first_nested_string(
                    &[mode.as_ref(), request_metadata.as_ref(), metadata.as_ref()],
                    &[
                        "displayName",
                        "display_name",
                        "name",
                        "title",
                        "label",
                        "modeName",
                        "mode_name",
                    ],
                )
            }),
            model_name: lookup.string(&["modelName", "model_name"]).or_else(|| {
                first_nested_string(
                    &[model.as_ref(), request_metadata.as_ref(), metadata.as_ref()],
                    &[
                        "displayName",
                        "display_name",
                        "name",
                        "title",
                        "label",
                        "modelName",
                        "model_name",
                    ],
                )
            }),
        })
    }
}

impl GrokConversationsResponse {
    pub fn from_raw_json(raw_json: Value) -> Self {
        let lookup = JsonLookup::new(raw_json.clone());
        let conversations = conversation_dictionaries(&raw_json)
            .into_iter()
            .filter_map(GrokConversation::from_dictionary)
            .collect();
        let text_search_matches = lookup
            .raw()
            .as_object()
            .and_then(|object| {
                object
                    .get("textSearchMatches")
                    .or_else(|| object.get("text_search_matches"))
            })
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(text_search_match)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Self {
            conversations,
            next_page_token: lookup.string(&["nextPageToken", "next_page_token"]),
            text_search_matches,
            raw_json,
        }
    }
}

impl GrokClient {
    pub async fn list_conversations_response(
        &self,
        options: &GrokConversationListOptions,
    ) -> Result<GrokConversationsResponse> {
        let request = self.list_conversations_request(options)?;
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokConversationsResponse::from_raw_json(raw_json))
    }

    pub async fn soft_delete_conversation(&self, conversation_id: &str) -> Result<()> {
        let request = self.soft_delete_conversation_request(conversation_id)?;
        self.send_request_json(request).await?;
        Ok(())
    }

    pub async fn get_conversation_v2(
        &self,
        conversation_id: &str,
        include_workspaces: bool,
        include_task_result: bool,
    ) -> Result<GrokConversationV2Response> {
        let request =
            self.conversation_v2_request(conversation_id, include_workspaces, include_task_result)?;
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokConversationV2Response::from_raw_json(raw_json))
    }

    pub async fn get_response_nodes(
        &self,
        conversation_id: &str,
        include_threads: bool,
    ) -> Result<Vec<GrokResponseNode>> {
        let request = self.response_nodes_request(conversation_id, include_threads)?;
        let raw_json = self.send_request_json(request).await?;
        Ok(parse_response_nodes(&raw_json))
    }

    pub async fn load_responses(
        &self,
        conversation_id: &str,
        specific_response_ids: Option<&[String]>,
    ) -> Result<Vec<GrokConversationMessage>> {
        let response_ids = match specific_response_ids {
            Some(ids) if !ids.is_empty() => ids.to_vec(),
            _ => self
                .get_response_nodes(conversation_id, false)
                .await
                .map(|nodes| {
                    nodes
                        .into_iter()
                        .map(|node| node.response_id)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        };

        let request = self.load_responses_request(conversation_id, &response_ids)?;
        let raw_json = self.send_request_json(request).await?;
        Ok(parse_conversation_messages(&raw_json))
    }
}

pub fn parse_response_nodes(value: &Value) -> Vec<GrokResponseNode> {
    response_node_dictionaries(value)
        .into_iter()
        .filter_map(GrokResponseNode::from_dictionary)
        .collect()
}

pub fn parse_conversation_messages(value: &Value) -> Vec<GrokConversationMessage> {
    response_dictionaries(value)
        .into_iter()
        .filter_map(GrokConversationMessage::from_dictionary)
        .collect()
}

fn conversation_dictionaries(value: &Value) -> Vec<Map<String, Value>> {
    match value {
        Value::Array(items) => items.iter().flat_map(direct_dictionaries).collect(),
        Value::Object(object) => {
            for key in ["conversations", "items"] {
                if let Some(nested) = object.get(key) {
                    let dictionaries = direct_dictionaries(nested);
                    if !dictionaries.is_empty() {
                        return dictionaries;
                    }
                }
            }

            for key in ["data", "result"] {
                if let Some(nested) = object.get(key) {
                    let dictionaries = direct_dictionaries(nested);
                    if !dictionaries.is_empty() && nested.as_array().is_some() {
                        return dictionaries;
                    }

                    let nested_dictionaries = conversation_dictionaries(nested);
                    if !nested_dictionaries.is_empty() {
                        return nested_dictionaries;
                    }
                }
            }

            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn direct_dictionaries(value: &Value) -> Vec<Map<String, Value>> {
    match value {
        Value::Array(items) => items.iter().flat_map(direct_dictionaries).collect(),
        Value::Object(object) => vec![object.clone()],
        _ => Vec::new(),
    }
}

fn response_node_dictionaries(value: &Value) -> Vec<Map<String, Value>> {
    match value {
        Value::Array(items) => items.iter().flat_map(direct_dictionaries).collect(),
        Value::Object(object) => {
            for key in ["responseNodes", "nodes", "responses"] {
                if let Some(nested) = object.get(key) {
                    let dictionaries = response_node_dictionaries_from_candidate(nested);
                    if !dictionaries.is_empty() {
                        return dictionaries;
                    }
                }
            }

            for key in ["data", "result", "payload"] {
                if let Some(nested) = object.get(key) {
                    let dictionaries = response_node_dictionaries(nested);
                    if !dictionaries.is_empty() {
                        return dictionaries;
                    }
                }
            }

            for nested in object.values() {
                let dictionaries = direct_dictionaries(nested);
                if nested.as_array().is_some()
                    && dictionaries.iter().any(is_response_node_dictionary)
                {
                    return dictionaries;
                }
            }

            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn response_node_dictionaries_from_candidate(value: &Value) -> Vec<Map<String, Value>> {
    let dictionaries = direct_dictionaries(value);
    if value.as_array().is_some() || dictionaries.iter().any(is_response_node_dictionary) {
        return dictionaries;
    }
    response_node_dictionaries(value)
}

fn is_response_node_dictionary(dictionary: &Map<String, Value>) -> bool {
    dictionary
        .get("responseId")
        .and_then(Value::as_str)
        .is_some()
        && dictionary.get("sender").and_then(Value::as_str).is_some()
}

fn response_dictionaries(value: &Value) -> Vec<Map<String, Value>> {
    match value {
        Value::Array(items) => items.iter().flat_map(direct_dictionaries).collect(),
        Value::Object(object) => {
            if let Some(nested) = object.get("responses") {
                let dictionaries = direct_dictionaries(nested);
                if !dictionaries.is_empty() {
                    return dictionaries;
                }
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn nested_dictionary(dictionary: &Map<String, Value>, keys: &[&str]) -> Option<Map<String, Value>> {
    keys.iter()
        .find_map(|key| dictionary.get(*key).and_then(Value::as_object).cloned())
}

fn first_nested_string(
    dictionaries: &[Option<&Map<String, Value>>],
    keys: &[&str],
) -> Option<String> {
    for dictionary in dictionaries.iter().flatten() {
        for key in keys {
            if let Some(value) = dictionary
                .get(*key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn text_search_match(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str()
        && !text.is_empty()
    {
        return Some(text.to_string());
    }

    let object = value.as_object()?;
    for key in ["text", "snippet", "message", "value"] {
        if let Some(text) = object.get(key).and_then(Value::as_str)
            && !text.is_empty()
        {
            return Some(text.to_string());
        }
    }
    None
}

fn conversation_v2_id(value: &Value, keys: &[&str]) -> Option<String> {
    let dictionary = value.as_object()?;
    keys.iter()
        .find_map(|key| dictionary.get(*key).and_then(scalar_description))
}

fn scalar_description(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => non_empty_string(value),
        Value::Number(value) => non_empty_string(&value.to_string()),
        Value::Bool(value) => non_empty_string(if *value { "true" } else { "false" }),
        _ => None,
    }
}

fn non_empty_string(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        GrokConversationV2Response, GrokConversationsResponse, parse_conversation_messages,
        parse_response_nodes,
    };

    #[test]
    fn parses_conversations_from_wrapped_response_like_swift_decoder() {
        let response = GrokConversationsResponse::from_raw_json(json!({
            "data": {
                "items": [
                    {
                        "conversation_id": "conv-1",
                        "name": "Research",
                        "starred": true,
                        "create_time": "2026-05-14T01:02:03Z",
                        "modify_time": "2026-05-14T02:03:04Z",
                        "system_prompt_name": "Default",
                        "temporary": true,
                        "media_types": ["image"],
                        "last_message": "Preview"
                    }
                ]
            },
            "next_page_token": "next",
            "text_search_matches": [{"snippet": "match"}]
        }));

        assert_eq!(response.conversations.len(), 1);
        assert_eq!(response.conversations[0].conversation_id, "conv-1");
        assert_eq!(response.conversations[0].title, "Research");
        assert!(response.conversations[0].starred);
        assert!(response.conversations[0].temporary);
        assert_eq!(response.conversations[0].media_types, ["image"]);
        assert_eq!(response.conversations[0].preview, "Preview");
        assert_eq!(response.next_page_token.as_deref(), Some("next"));
        assert_eq!(response.text_search_matches, ["match"]);
    }

    #[test]
    fn parses_old_array_response_like_swift_fallback() {
        let response = GrokConversationsResponse::from_raw_json(json!([
            {
                "id": "conv-1",
                "title": "Array response"
            }
        ]));

        assert_eq!(response.conversations.len(), 1);
        assert_eq!(response.conversations[0].conversation_id, "conv-1");
        assert_eq!(response.conversations[0].title, "Array response");
    }

    #[test]
    fn parses_conversation_v2_detail_from_common_wrappers() {
        let response = GrokConversationV2Response::from_raw_json(json!({
            "conversation": {
                "conversation_id": "conv-1",
                "title": "Research",
                "workspaces": ["workspace-1"],
                "task_result": {"status": "done"}
            }
        }));

        assert_eq!(response.conversation_id.as_deref(), Some("conv-1"));
    }

    #[test]
    fn conversation_v2_id_accepts_scalar_descriptions_like_swift() {
        let numeric = GrokConversationV2Response::from_raw_json(json!({
            "result": {
                "id": 42,
                "title": "Numeric id"
            }
        }));
        let boolean = GrokConversationV2Response::from_raw_json(json!({
            "conversation": {
                "conversationId": true,
                "title": "Boolean id"
            }
        }));

        assert_eq!(numeric.conversation_id.as_deref(), Some("42"));
        assert_eq!(boolean.conversation_id.as_deref(), Some("true"));
    }

    #[test]
    fn parses_response_nodes_from_common_wrappers() {
        let nodes = parse_response_nodes(&json!({
            "nodes": [
                {
                    "responseId": "resp-1",
                    "sender": "human"
                },
                {
                    "responseId": "resp-2",
                    "sender": "assistant",
                    "parentResponseId": "resp-1"
                }
            ]
        }));

        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].response_id, "resp-1");
        assert_eq!(nodes[0].sender, "human");
        assert_eq!(nodes[1].parent_response_id.as_deref(), Some("resp-1"));
    }

    #[test]
    fn parses_response_nodes_from_nested_wrappers() {
        let nodes = parse_response_nodes(&json!({
            "data": {
                "result": {
                    "responseNodes": [
                        {
                            "responseId": "resp-1",
                            "sender": "human"
                        },
                        {
                            "responseId": "resp-2",
                            "sender": "assistant",
                            "parentResponseId": "resp-1"
                        }
                    ]
                }
            }
        }));

        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].response_id, "resp-1");
        assert_eq!(nodes[1].response_id, "resp-2");
        assert_eq!(nodes[1].parent_response_id.as_deref(), Some("resp-1"));
    }

    #[test]
    fn response_node_fallback_skips_unrelated_arrays_like_swift() {
        let nodes = parse_response_nodes(&json!({
            "metadata": [
                {"id": "not-a-response-node"}
            ],
            "payload": [
                {
                    "responseId": "resp-1",
                    "sender": "human"
                },
                {
                    "responseId": "resp-2",
                    "sender": "assistant",
                    "parentResponseId": "resp-1"
                }
            ]
        }));

        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].response_id, "resp-1");
        assert_eq!(nodes[1].parent_response_id.as_deref(), Some("resp-1"));
    }

    #[test]
    fn parses_conversation_messages_with_metadata_aliases() {
        let messages = parse_conversation_messages(&json!({
            "responses": [
                {
                    "responseId": "resp-1",
                    "sender": "human",
                    "message": "Hello",
                    "createTime": "2026-05-14T01:02:03Z"
                },
                {
                    "responseId": "resp-2",
                    "sender": "assistant",
                    "message": "Hi",
                    "createTime": "2026-05-14T01:02:04Z",
                    "parentResponseId": "resp-1",
                    "metadata": {
                        "request_metadata": {
                            "mode_id": "fast",
                            "display_name": "Fast"
                        }
                    }
                }
            ]
        }));

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].response_id, "resp-2");
        assert_eq!(messages[1].message, "Hi");
        assert_eq!(messages[1].parent_response_id.as_deref(), Some("resp-1"));
        assert_eq!(messages[1].mode_id.as_deref(), Some("fast"));
        assert_eq!(messages[1].mode_name.as_deref(), Some("Fast"));
    }
}
