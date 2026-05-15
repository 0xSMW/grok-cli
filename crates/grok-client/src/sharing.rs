use serde_json::{Map, Value};

use crate::{GrokClient, GrokError, Result, encoded_path_segment};

impl GrokClient {
    pub async fn share_link_url(
        &self,
        conversation_id: &str,
        response_id: Option<&str>,
        options: &crate::GrokShareLinkOptions,
    ) -> Result<String> {
        let request = self.share_links_request(conversation_id, response_id, options)?;
        let raw_json = self.send_request_json(request).await?;
        if let Some(url) = first_share_link_url(&raw_json) {
            return Ok(url);
        }

        let Some(response_id) = response_id.filter(|value| !value.trim().is_empty()) else {
            return Err(GrokError::Api(
                "No existing share link found and no response ID available to create one"
                    .to_string(),
            ));
        };

        self.create_share_link_url(conversation_id, response_id, options)
            .await
    }

    pub async fn create_share_link_url(
        &self,
        conversation_id: &str,
        response_id: &str,
        options: &crate::GrokShareLinkOptions,
    ) -> Result<String> {
        let request = self.create_share_link_request(conversation_id, response_id, options)?;
        let raw_json = self.send_request_json(request).await?;
        first_share_link_url(&raw_json).ok_or_else(|| {
            GrokError::Api("Share link response did not include a share URL".to_string())
        })
    }
}

pub fn first_share_link_url(value: &Value) -> Option<String> {
    match value {
        Value::Object(dictionary) => {
            if let Some(url) = share_link_url_from_dictionary(dictionary) {
                return Some(url);
            }

            for key in SHARING_WRAPPER_KEYS {
                if let Some(nested) = dictionary.get(*key)
                    && let Some(url) = first_share_link_url(nested)
                {
                    return Some(url);
                }
            }

            dictionary.values().find_map(first_share_link_url)
        }
        Value::Array(values) => values.iter().find_map(first_share_link_url),
        _ => None,
    }
}

fn share_link_url_from_dictionary(dictionary: &Map<String, Value>) -> Option<String> {
    if let Some(url) = first_string_in_dictionary(dictionary, SHARING_URL_KEYS)
        .and_then(|value| clean_share_link_url(&value))
    {
        return Some(url);
    }

    first_string_in_dictionary(dictionary, SHARING_ID_KEYS)
        .and_then(|value| share_link_url_from_identifier(&value))
}

fn clean_share_link_url(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        return Some(trimmed.to_string());
    }
    if trimmed.starts_with("grok.com/") {
        return Some(format!("https://{trimmed}"));
    }
    if trimmed.starts_with("/share/") {
        return Some(format!("https://grok.com{trimmed}"));
    }
    share_link_url_from_identifier(trimmed)
}

fn share_link_url_from_identifier(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        return Some(trimmed.to_string());
    }
    if trimmed.starts_with("grok.com/") {
        return Some(format!("https://{trimmed}"));
    }
    if trimmed.starts_with("/share/") {
        return Some(format!("https://grok.com{trimmed}"));
    }
    Some(format!(
        "https://grok.com/share/{}",
        encoded_path_segment(trimmed)
    ))
}

fn first_string_in_dictionary(dictionary: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = dictionary
            .get(*key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            return Some(value.to_string());
        }
    }

    dictionary.values().find_map(|nested| match nested {
        Value::Object(nested) => first_string_in_dictionary(nested, keys),
        Value::Array(values) => values.iter().find_map(|value| match value {
            Value::Object(nested) => first_string_in_dictionary(nested, keys),
            _ => None,
        }),
        _ => None,
    })
}

const SHARING_WRAPPER_KEYS: &[&str] = &["shareLinks", "share_links", "items", "result", "data"];
const SHARING_URL_KEYS: &[&str] = &[
    "url",
    "shareUrl",
    "share_url",
    "link",
    "shareLink",
    "share_link",
];
const SHARING_ID_KEYS: &[&str] = &[
    "publicId",
    "public_id",
    "token",
    "id",
    "shareId",
    "share_id",
    "shareLinkId",
    "share_link_id",
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc::{self, Receiver};
    use std::thread;
    use std::time::Duration;

    use serde_json::json;

    use super::first_share_link_url;
    use crate::{GrokClient, GrokShareLinkOptions};

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct RecordedRequest {
        method: String,
        path: String,
        body: String,
    }

    struct JsonMockServer {
        base_url: String,
        receiver: Receiver<RecordedRequest>,
        handle: thread::JoinHandle<std::io::Result<()>>,
    }

    impl JsonMockServer {
        fn spawn_sequence(bodies: Vec<&'static str>) -> std::io::Result<Self> {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let address = listener.local_addr()?;
            let (sender, receiver) = mpsc::channel();
            let handle = thread::spawn(move || -> std::io::Result<()> {
                for body in bodies {
                    let (mut stream, _) = listener.accept()?;
                    let mut buffer = [0_u8; 8192];
                    let bytes_read = stream.read(&mut buffer)?;
                    let mut request_bytes = buffer[..bytes_read].to_vec();
                    let headers_end = request_bytes
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .map(|index| index + 4);
                    let content_length = headers_end
                        .and_then(|end| std::str::from_utf8(&request_bytes[..end]).ok())
                        .and_then(content_length);
                    if let (Some(headers_end), Some(content_length)) = (headers_end, content_length)
                    {
                        while request_bytes.len().saturating_sub(headers_end) < content_length {
                            let bytes_read = stream.read(&mut buffer)?;
                            if bytes_read == 0 {
                                break;
                            }
                            request_bytes.extend_from_slice(&buffer[..bytes_read]);
                        }
                    }

                    let request = String::from_utf8_lossy(&request_bytes).to_string();
                    let request_line = request.lines().next().ok_or_else(|| {
                        std::io::Error::new(std::io::ErrorKind::InvalidData, "missing request line")
                    })?;
                    let mut parts = request_line.split_whitespace();
                    let method = parts
                        .next()
                        .ok_or_else(|| {
                            std::io::Error::new(std::io::ErrorKind::InvalidData, "missing method")
                        })?
                        .to_string();
                    let path = parts
                        .next()
                        .ok_or_else(|| {
                            std::io::Error::new(std::io::ErrorKind::InvalidData, "missing path")
                        })?
                        .to_string();
                    let recorded_body = request
                        .split_once("\r\n\r\n")
                        .map(|(_, body)| body.to_string())
                        .unwrap_or_default();
                    sender
                        .send(RecordedRequest {
                            method,
                            path,
                            body: recorded_body,
                        })
                        .map_err(|_| {
                            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "receiver closed")
                        })?;

                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream.write_all(response.as_bytes())?;
                }
                Ok(())
            });

            Ok(Self {
                base_url: format!("http://{address}/rest"),
                receiver,
                handle,
            })
        }

        fn recorded_requests(self, count: usize) -> std::io::Result<Vec<RecordedRequest>> {
            let mut requests = Vec::new();
            for _ in 0..count {
                requests.push(
                    self.receiver
                        .recv_timeout(Duration::from_secs(5))
                        .map_err(|error| std::io::Error::other(error.to_string()))?,
                );
            }
            let join_result = self
                .handle
                .join()
                .map_err(|_| std::io::Error::other("mock server thread panicked"))?;
            join_result?;
            Ok(requests)
        }
    }

    fn content_length(headers: &str) -> Option<usize> {
        headers.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())
                .flatten()
        })
    }

    #[test]
    fn parses_existing_share_url_from_wrappers() {
        let url = first_share_link_url(&json!({
            "result": {
                "share_links": [
                    {
                        "conversationId": "conv 123",
                        "share_url": " https://grok.com/share/share_abc123 "
                    }
                ]
            }
        }));

        assert_eq!(url.as_deref(), Some("https://grok.com/share/share_abc123"));
    }

    #[test]
    fn constructs_share_url_from_identifier() {
        let url = first_share_link_url(&json!({
            "data": {
                "items": [
                    {
                        "share_link_id": "share space/slash?and&unicode東京"
                    }
                ]
            }
        }));

        assert_eq!(
            url.as_deref(),
            Some("https://grok.com/share/share%20space%2Fslash%3Fand%26unicode%E6%9D%B1%E4%BA%AC")
        );
    }

    #[tokio::test]
    async fn share_link_url_creates_link_when_lookup_is_empty()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = JsonMockServer::spawn_sequence(vec![
            r#"{"shareLinks":[]}"#,
            r#"{"shareLinkId":"created_share_123"}"#,
        ])?;
        let client = GrokClient::with_options(
            BTreeMap::from([("sso".to_string(), "cookie".to_string())]),
            false,
            Some(&server.base_url),
        )?;

        let share_url = client
            .share_link_url(
                "conv create",
                Some("resp 1"),
                &GrokShareLinkOptions::default(),
            )
            .await?;
        let requests = server.recorded_requests(2)?;
        let create_body: serde_json::Value = serde_json::from_str(&requests[1].body)?;

        assert_eq!(share_url, "https://grok.com/share/created_share_123");
        assert_eq!(requests[0].method, "GET");
        assert_eq!(
            requests[0].path,
            "/rest/app-chat/share_links?pageSize=100&conversationId=conv%20create&responseId=resp%201"
        );
        assert_eq!(requests[1].method, "POST");
        assert_eq!(
            requests[1].path,
            "/rest/app-chat/conversations/conv%20create/share"
        );
        assert_eq!(create_body["responseId"], "resp 1");
        assert_eq!(create_body["allowIndexing"], true);
        Ok(())
    }

    #[tokio::test]
    async fn share_link_url_encodes_special_characters_through_lookup_and_create()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = JsonMockServer::spawn_sequence(vec![
            r#"{"shareLinks":[]}"#,
            r#"{"shareLinkId":"share space/slash?and&unicode東京"}"#,
        ])?;
        let client = GrokClient::with_options(
            BTreeMap::from([("sso".to_string(), "cookie".to_string())]),
            false,
            Some(&server.base_url),
        )?;
        let options = GrokShareLinkOptions {
            page_size: 9,
            allow_indexing: false,
        };

        let share_url = client
            .share_link_url(
                "conv space/slash?and&unicode東京",
                Some("resp space/slash?and&unicode東京"),
                &options,
            )
            .await?;
        let requests = server.recorded_requests(2)?;
        let create_body: serde_json::Value = serde_json::from_str(&requests[1].body)?;

        assert_eq!(
            share_url,
            "https://grok.com/share/share%20space%2Fslash%3Fand%26unicode%E6%9D%B1%E4%BA%AC"
        );
        assert_eq!(requests[0].method, "GET");
        assert_eq!(
            requests[0].path,
            "/rest/app-chat/share_links?pageSize=9&conversationId=conv%20space/slash?and%26unicode%E6%9D%B1%E4%BA%AC&responseId=resp%20space/slash?and%26unicode%E6%9D%B1%E4%BA%AC"
        );
        assert_eq!(requests[1].method, "POST");
        assert_eq!(
            requests[1].path,
            "/rest/app-chat/conversations/conv%20space%2Fslash%3Fand%26unicode%E6%9D%B1%E4%BA%AC/share"
        );
        assert_eq!(
            create_body["responseId"],
            "resp space/slash?and&unicode東京"
        );
        assert_eq!(create_body["allowIndexing"], false);
        Ok(())
    }
}
