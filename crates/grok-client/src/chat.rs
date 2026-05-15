use reqwest::Method;
use serde_json::{Map, Value, json};

use crate::{
    ConversationResponse, GrokClient, GrokError, GrokMessageOptions, GrokRequest, GrokStreamParser,
    RestNamespace, Result, endpoint_path,
};

impl GrokClient {
    pub fn new_conversation_request(
        &self,
        message: &str,
        options: &GrokMessageOptions,
    ) -> Result<GrokRequest> {
        self.make_request(
            "/conversations/new",
            Method::POST,
            Some(message_payload(message, options)),
            RestNamespace::AppChat,
        )
    }

    pub fn continue_conversation_request(
        &self,
        conversation_id: &str,
        parent_response_id: Option<&str>,
        message: &str,
        options: &GrokMessageOptions,
    ) -> Result<GrokRequest> {
        let mut payload = message_payload(message, options);
        if let Some(parent_response_id) = parent_response_id
            && let Some(object) = payload.as_object_mut()
        {
            object.insert("parentResponseId".to_string(), json!(parent_response_id));
        }
        let path = endpoint_path(&["conversations", conversation_id, "responses"], &[])?;
        self.make_request(&path, Method::POST, Some(payload), RestNamespace::AppChat)
    }

    pub async fn send_message_response(
        &self,
        message: &str,
        options: &GrokMessageOptions,
    ) -> Result<ConversationResponse> {
        let request = self.new_conversation_request(message, options)?;
        let body = self.send_request_text(request).await?;
        final_response_from_stream_text(&body, "")
    }

    pub async fn continue_conversation_response(
        &self,
        conversation_id: &str,
        parent_response_id: Option<&str>,
        message: &str,
        options: &GrokMessageOptions,
    ) -> Result<ConversationResponse> {
        let request = self.continue_conversation_request(
            conversation_id,
            parent_response_id,
            message,
            options,
        )?;
        let body = self.send_request_text(request).await?;
        final_response_from_stream_text(&body, conversation_id)
    }

    pub async fn stream_message_responses<F>(
        &self,
        message: &str,
        options: &GrokMessageOptions,
        mut on_response: F,
    ) -> Result<()>
    where
        F: FnMut(ConversationResponse) -> Result<()>,
    {
        let request = self.new_conversation_request(message, options)?;
        let mut parser = GrokStreamParser::new("");
        self.stream_request_lines_with_mode(request, Some(&options.mode_id), |line| {
            let response = parser.consume_line(&line);
            match response {
                Ok(Some(response)) => on_response(response),
                Ok(None) => Ok(()),
                Err(error) => Err(error),
            }
        })
        .await?;

        if let Some(response) = parser.finish() {
            on_response(response)?;
        }

        Ok(())
    }
}

pub fn message_payload(message: &str, options: &GrokMessageOptions) -> Value {
    let mut payload = Map::new();
    payload.insert("temporary".to_string(), json!(options.temporary));
    payload.insert("message".to_string(), json!(message));
    payload.insert("modeId".to_string(), json!(options.mode_id));
    payload.insert("imageAttachments".to_string(), json!([]));
    payload.insert(
        "fileAttachments".to_string(),
        json!(options.file_attachments),
    );
    payload.insert("enableImageGeneration".to_string(), json!(true));
    payload.insert("returnImageBytes".to_string(), json!(false));
    payload.insert("returnRawGrokInXaiRequest".to_string(), json!(false));
    payload.insert("enableImageStreaming".to_string(), json!(true));
    payload.insert("imageGenerationCount".to_string(), json!(2));
    payload.insert("forceConcise".to_string(), json!(false));
    payload.insert("enableSideBySide".to_string(), json!(true));
    payload.insert("sendFinalMetadata".to_string(), json!(true));
    payload.insert("disableTextFollowUps".to_string(), json!(false));
    payload.insert("responseMetadata".to_string(), json!({}));
    payload.insert("disableMemory".to_string(), json!(false));
    payload.insert("forceSideBySide".to_string(), json!(false));
    payload.insert("isAsyncChat".to_string(), json!(false));
    payload.insert("disableSelfHarmShortCircuit".to_string(), json!(false));
    payload.insert("collectionIds".to_string(), json!([]));
    payload.insert(
        "disabledConnectorIds".to_string(),
        json!(options.disabled_connector_ids),
    );
    payload.insert("deviceEnvInfo".to_string(), device_env_info());
    if !options.workspace_ids.is_empty() {
        payload.insert("workspaceIds".to_string(), json!(options.workspace_ids));
    }
    Value::Object(payload)
}

pub fn final_response_from_stream_text(
    stream_text: &str,
    initial_conversation_id: &str,
) -> Result<ConversationResponse> {
    let mut parser = GrokStreamParser::new(initial_conversation_id);
    let mut accumulated = String::new();
    let mut latest = None;

    for line in stream_text.lines() {
        let Some(response) = parser.consume_line(line)? else {
            continue;
        };
        if response.is_final {
            return Ok(response);
        }
        accumulated.push_str(&response.message);
        latest = Some(response);
    }

    if let Some(response) = parser.finish() {
        return Ok(response);
    }

    if let Some(response) = latest {
        return Ok(ConversationResponse::final_message(
            accumulated.trim().to_string(),
            response.conversation_id,
            response.response_id,
            None,
            None,
            false,
        ));
    }

    Err(GrokError::Streaming(
        "Stream ended without a response".to_string(),
    ))
}

fn device_env_info() -> Value {
    json!({
        "darkModeEnabled": true,
        "devicePixelRatio": 2,
        "screenWidth": 1728,
        "screenHeight": 1117,
        "viewportWidth": 1728,
        "viewportHeight": 564
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    use super::{final_response_from_stream_text, message_payload};
    use crate::{GrokClient, GrokError, GrokMessageOptions, GrokPersonalityType, Result};

    #[test]
    fn message_payload_matches_swift_new_conversation_contract() {
        let options = GrokMessageOptions {
            temporary: true,
            mode_id: "grok-special".to_string(),
            file_attachments: vec!["file-1".to_string()],
            workspace_ids: vec!["workspace-1".to_string()],
            disabled_connector_ids: vec!["connector-1".to_string()],
            ..GrokMessageOptions::default()
        };

        let payload = message_payload("hello", &options);

        assert_eq!(payload["temporary"], true);
        assert_eq!(payload["message"], "hello");
        assert_eq!(payload["modeId"], "grok-special");
        assert_eq!(payload["imageAttachments"], json!([]));
        assert_eq!(payload["fileAttachments"], json!(["file-1"]));
        assert_eq!(payload["enableImageGeneration"], true);
        assert_eq!(payload["returnImageBytes"], false);
        assert_eq!(payload["enableImageStreaming"], true);
        assert_eq!(payload["imageGenerationCount"], 2);
        assert_eq!(payload["sendFinalMetadata"], true);
        assert_eq!(payload["disabledConnectorIds"], json!(["connector-1"]));
        assert_eq!(payload["workspaceIds"], json!(["workspace-1"]));
        assert_eq!(payload["deviceEnvInfo"]["screenWidth"], 1728);
    }

    #[test]
    fn message_payload_omits_empty_workspace_ids_like_swift() {
        let payload = message_payload("hello", &GrokMessageOptions::default());

        assert!(payload.get("workspaceIds").is_none());
    }

    #[test]
    fn message_payload_omits_deprecated_personality_type_like_swift() {
        let options = GrokMessageOptions {
            personality_type: GrokPersonalityType::Romance,
            ..GrokMessageOptions::default()
        };

        let payload = message_payload("hello", &options);

        assert!(payload.get("personalityType").is_none());
    }

    #[test]
    fn continue_conversation_request_matches_swift_contract() -> Result<()> {
        let client = GrokClient::with_options(
            std::collections::BTreeMap::from([("sso".to_string(), "cookie".to_string())]),
            false,
            Some("https://example.test/rest"),
        )?;
        let request = client.continue_conversation_request(
            "conv space/slash?and&unicode東京",
            Some("resp-parent"),
            "follow up",
            &GrokMessageOptions::default(),
        )?;

        assert_eq!(request.method, reqwest::Method::POST);
        assert_eq!(
            request.url,
            "https://example.test/rest/app-chat/conversations/conv%20space%2Fslash%3Fand%26unicode%E6%9D%B1%E4%BA%AC/responses"
        );
        assert_eq!(
            request.body.as_ref().and_then(|body| body.get("message")),
            Some(&json!("follow up"))
        );
        assert_eq!(
            request
                .body
                .as_ref()
                .and_then(|body| body.get("parentResponseId")),
            Some(&json!("resp-parent"))
        );
        Ok(())
    }

    #[test]
    fn final_response_drains_sse_text() -> Result<()> {
        let response = final_response_from_stream_text(
            r#"data: {"result":{"conversation":{"conversationId":"conv-1"},"response":{"responseId":"resp-1","token":"Hello "}}}
data: {"result":{"response":{"responseId":"resp-1","token":"world"}}}
data: {"result":{"response":{"modelResponse":{"responseId":"resp-1","message":"Hello world"}}}}
"#,
            "",
        )?;

        assert_eq!(response.message, "Hello world");
        assert_eq!(response.conversation_id, "conv-1");
        assert_eq!(response.response_id, "resp-1");
        assert!(response.is_final);
        Ok(())
    }

    #[tokio::test]
    async fn stream_message_responses_reads_upstream_byte_stream()
    -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let handle = thread::spawn(move || -> std::io::Result<String> {
            let (mut stream, _) = listener.accept()?;
            let mut request_bytes = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let bytes_read = stream.read(&mut buffer)?;
                if bytes_read == 0 {
                    break;
                }
                request_bytes.extend_from_slice(&buffer[..bytes_read]);
                let Some(headers_end) = request_bytes
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|index| index + 4)
                else {
                    continue;
                };
                let content_length = std::str::from_utf8(&request_bytes[..headers_end])
                    .ok()
                    .and_then(content_length)
                    .unwrap_or(0);
                if request_bytes.len().saturating_sub(headers_end) >= content_length {
                    break;
                }
            }

            let body = concat!(
                "data: {\"result\":{\"conversation\":{\"conversationId\":\"conv-1\"},\"response\":{\"responseId\":\"resp-1\",\"token\":\"Hel\"}}}\n",
                "data: {\"result\":{\"response\":{\"responseId\":\"resp-1\",\"token\":\"lo\"}}}\n",
                "data: {\"result\":{\"response\":{\"modelResponse\":{\"responseId\":\"resp-1\",\"message\":\"Hello\"}}}}\n"
            );
            let headers = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(headers.as_bytes())?;
            for chunk in body.as_bytes().chunks(17) {
                stream.write_all(chunk)?;
                stream.flush()?;
                thread::sleep(Duration::from_millis(2));
            }
            Ok(String::from_utf8_lossy(&request_bytes).to_string())
        });

        let client = GrokClient::with_options(
            std::collections::BTreeMap::from([("sso".to_string(), "cookie".to_string())]),
            false,
            Some(&format!("http://{address}/rest")),
        )?;
        let mut responses = Vec::new();
        client
            .stream_message_responses("hello", &GrokMessageOptions::default(), |response| {
                responses.push(response);
                Ok(())
            })
            .await?;
        let request = handle
            .join()
            .map_err(|_| std::io::Error::other("server thread panicked"))??;

        assert!(request.starts_with("POST /rest/app-chat/conversations/new HTTP/1.1"));
        assert_eq!(responses.len(), 3);
        assert_eq!(responses[0].message, "Hel");
        assert_eq!(responses[1].message, "lo");
        assert_eq!(responses[2].message, "Hello");
        assert!(responses[2].is_final);
        Ok(())
    }

    #[tokio::test]
    async fn stream_message_responses_reports_access_denied_for_selected_mode()
    -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let handle = thread::spawn(move || -> std::io::Result<()> {
            let (mut stream, _) = listener.accept()?;
            let mut request_bytes = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let bytes_read = stream.read(&mut buffer)?;
                if bytes_read == 0 {
                    break;
                }
                request_bytes.extend_from_slice(&buffer[..bytes_read]);
                let Some(headers_end) = request_bytes
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|index| index + 4)
                else {
                    continue;
                };
                let content_length = std::str::from_utf8(&request_bytes[..headers_end])
                    .ok()
                    .and_then(content_length)
                    .unwrap_or(0);
                if request_bytes.len().saturating_sub(headers_end) >= content_length {
                    break;
                }
            }

            let body = r#"{"error":"forbidden"}"#;
            let headers = format!(
                "HTTP/1.1 403 Forbidden\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(headers.as_bytes())?;
            stream.write_all(body.as_bytes())?;
            Ok(())
        });

        let client = GrokClient::with_options(
            std::collections::BTreeMap::from([("sso".to_string(), "cookie".to_string())]),
            false,
            Some(&format!("http://{address}/rest")),
        )?;
        let options = GrokMessageOptions {
            mode_id: "expert".to_string(),
            ..GrokMessageOptions::default()
        };
        let result = client
            .stream_message_responses("hello", &options, |_| Ok(()))
            .await;

        handle
            .join()
            .map_err(|_| std::io::Error::other("server thread panicked"))??;

        match result {
            Err(GrokError::AccessDenied(message)) => {
                assert!(message.contains("Grok denied access to Expert (expert)."));
            }
            other => panic!("expected selected-mode access denied error, got {other:?}"),
        }
        Ok(())
    }

    fn content_length(headers: &str) -> Option<usize> {
        headers.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())
                .flatten()
        })
    }
}
