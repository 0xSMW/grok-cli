use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose};
use futures_util::StreamExt;
use reqwest::Method;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{GrokError, Result, StreamingLineReader, validate_http_response};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestNamespace {
    AppChat,
    Root,
    Web,
}

impl RestNamespace {
    pub fn path_prefix(self) -> &'static str {
        match self {
            Self::AppChat => "/rest/app-chat",
            Self::Root => "/rest",
            Self::Web => "",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokRequest {
    pub method: Method,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Option<Value>,
}

impl GrokRequest {
    pub fn curl_representation(&self, redact_cookies: bool) -> String {
        let mut components = vec!["curl".to_string()];
        if self.method != Method::GET {
            components.push(format!("-X {}", self.method));
        }
        for (name, value) in &self.headers {
            let header_value = if redact_cookies && is_sensitive_header(name) {
                "<redacted>"
            } else {
                value
            };
            components.push(format!("-H \"{name}: {header_value}\""));
        }
        if let Some(body) = self.body.as_ref() {
            let body = redacted_body_string(body);
            let escaped_body = body.replace('\'', "'\\''");
            components.push(format!("--data '{escaped_body}'"));
        }
        components.push(format!("\"{}\"", self.url));
        components.join(" ")
    }
}

pub struct GrokClient {
    base_url: String,
    root_base_url: String,
    web_base_url: String,
    cookies: BTreeMap<String, String>,
    http: reqwest::Client,
    pub is_debug: bool,
}

fn is_sensitive_header(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "authorization" | "cookie" | "proxy-authorization" | "set-cookie"
    )
}

fn redacted_body_string(body: &Value) -> String {
    let redacted = redacted_json_value(body).unwrap_or_else(|| body.clone());
    serde_json::to_string(&redacted).unwrap_or_else(|_| body.to_string())
}

fn redacted_json_value(value: &Value) -> Option<Value> {
    match value {
        Value::Object(dictionary) => {
            let mut redacted = serde_json::Map::new();
            let mut changed = false;
            for (key, nested_value) in dictionary {
                if is_sensitive_json_key(key) {
                    redacted.insert(
                        key.clone(),
                        Value::String(redacted_description(nested_value)),
                    );
                    changed = true;
                } else if let Some(nested_redacted) = redacted_json_value(nested_value) {
                    redacted.insert(key.clone(), nested_redacted);
                    changed = true;
                } else {
                    redacted.insert(key.clone(), nested_value.clone());
                }
            }
            changed.then_some(Value::Object(redacted))
        }
        Value::Array(values) => {
            let mut changed = false;
            let redacted = values
                .iter()
                .map(|value| {
                    if let Some(nested_redacted) = redacted_json_value(value) {
                        changed = true;
                        nested_redacted
                    } else {
                        value.clone()
                    }
                })
                .collect::<Vec<_>>();
            changed.then_some(Value::Array(redacted))
        }
        _ => None,
    }
}

fn is_sensitive_json_key(key: &str) -> bool {
    matches!(
        key.to_lowercase().as_str(),
        "audiobase64" | "content" | "data" | "file"
    )
}

fn redacted_description(value: &Value) -> String {
    match value {
        Value::String(value) => format!("<redacted {} chars>", value.chars().count()),
        Value::Array(values) => format!("<redacted {} items>", values.len()),
        Value::Object(dictionary) => format!("<redacted {} fields>", dictionary.len()),
        _ => "<redacted>".to_string(),
    }
}

impl GrokClient {
    pub fn new(cookies: BTreeMap<String, String>) -> Result<Self> {
        Self::with_options(cookies, false, None)
    }

    pub fn with_options(
        cookies: BTreeMap<String, String>,
        is_debug: bool,
        configured_base_url: Option<&str>,
    ) -> Result<Self> {
        if cookies.is_empty() {
            return Err(GrokError::InvalidCredentials);
        }

        let (base_url, root_base_url, web_base_url) = normalized_base_urls(configured_base_url);
        Ok(Self {
            base_url,
            root_base_url,
            web_base_url,
            cookies,
            http: reqwest::Client::new(),
            is_debug,
        })
    }

    pub fn from_json_file(
        path: impl AsRef<Path>,
        is_debug: bool,
        configured_base_url: Option<&str>,
    ) -> Result<Self> {
        let bytes = fs::read(path).map_err(|error| GrokError::Api(error.to_string()))?;
        let cookies = serde_json::from_slice::<BTreeMap<String, String>>(&bytes)?;
        Self::with_options(cookies, is_debug, configured_base_url)
    }

    pub fn app_chat_base_url(&self) -> &str {
        &self.base_url
    }

    pub fn root_base_url(&self) -> &str {
        &self.root_base_url
    }

    pub fn web_base_url(&self) -> &str {
        &self.web_base_url
    }

    pub fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    pub fn make_request(
        &self,
        path: &str,
        method: Method,
        payload: Option<Value>,
        namespace: RestNamespace,
    ) -> Result<GrokRequest> {
        let request_base_url = match namespace {
            RestNamespace::AppChat => &self.base_url,
            RestNamespace::Root => &self.root_base_url,
            RestNamespace::Web => &self.web_base_url,
        };
        let url = format!("{request_base_url}{path}");

        let mut headers = browser_headers();
        if payload.is_none() {
            headers.remove("content-type");
            headers.remove("origin");
        }
        headers.insert(
            "x-xai-request-id".to_string(),
            Uuid::new_v4().hyphenated().to_string().to_lowercase(),
        );
        if let Some(statsig_id) = make_statsig_id(path, &method, namespace) {
            headers.insert("x-statsig-id".to_string(), statsig_id);
        }
        headers.insert("Cookie".to_string(), self.cookie_header());

        Ok(GrokRequest {
            method,
            url,
            headers,
            body: payload,
        })
    }

    pub async fn send_json(
        &self,
        path: &str,
        method: Method,
        payload: Option<Value>,
        namespace: RestNamespace,
    ) -> Result<Value> {
        let request = self.make_request(path, method.clone(), payload, namespace)?;
        self.send_request_json(request).await
    }

    pub async fn send_request_json(&self, request: GrokRequest) -> Result<Value> {
        let mut builder = self.http.request(request.method.clone(), &request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = request.body {
            builder = builder.json(&body);
        }

        let response = builder.send().await?;
        let status = response.status();
        let body = response.bytes().await?;
        if !status.is_success() {
            validate_http_response(status.as_u16(), &body, None)?;
        }
        if body.is_empty() {
            return Ok(Value::Object(Default::default()));
        }
        serde_json::from_slice(&body).map_err(Into::into)
    }

    pub async fn send_request_text(&self, request: GrokRequest) -> Result<String> {
        let mut builder = self.http.request(request.method.clone(), &request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = request.body {
            builder = builder.json(&body);
        }

        let response = builder.send().await?;
        let status = response.status();
        let body = response.bytes().await?;
        if !status.is_success() {
            validate_http_response(status.as_u16(), &body, None)?;
        }
        String::from_utf8(body.to_vec()).map_err(|error| GrokError::Decoding(error.to_string()))
    }

    pub async fn stream_request_lines<F>(&self, request: GrokRequest, on_line: F) -> Result<()>
    where
        F: FnMut(String) -> Result<()>,
    {
        self.stream_request_lines_with_mode(request, None, on_line)
            .await
    }

    pub async fn stream_request_lines_with_mode<F>(
        &self,
        request: GrokRequest,
        mode_id: Option<&str>,
        mut on_line: F,
    ) -> Result<()>
    where
        F: FnMut(String) -> Result<()>,
    {
        let mut builder = self.http.request(request.method.clone(), &request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = request.body {
            builder = builder.json(&body);
        }

        let response = builder.send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.bytes().await?;
            return validate_http_response(status.as_u16(), &body, mode_id);
        }

        let mut reader = StreamingLineReader::default();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            for line in reader.append_utf8(&chunk)? {
                on_line(line)?;
            }
        }

        if let Some(line) = reader.flush_partial_utf8()? {
            on_line(line)?;
        }

        Ok(())
    }
}

fn statsig_path(path: &str, namespace: RestNamespace) -> String {
    let path_without_query = path.split_once('?').map(|(path, _)| path).unwrap_or(path);
    format!("{}{}", namespace.path_prefix(), path_without_query)
}

fn make_statsig_id(path: &str, method: &Method, namespace: RestNamespace) -> Option<String> {
    let meta_base64 = "aTdepyfBsvO5OewwurJnUTpd+p89iA3b26j9Sw2BhK32z+fmV5t8Qxe91l75WsOp";
    let fingerprint = "90e5cb100a3d70a3d70a3d800a3d70a3d70a3d8100";
    let meta_bytes = general_purpose::STANDARD.decode(meta_base64).ok()?;
    let epoch_offset = 0x644f6370_u64;
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let relative_seconds = seconds.saturating_sub(epoch_offset).min(u32::MAX as u64) as u32;
    let message = format!(
        "{}!{}!{}obfiowerehiring{}",
        method.as_str(),
        statsig_path(path, namespace),
        relative_seconds,
        fingerprint
    );
    let digest = Sha256::digest(message.as_bytes());
    let random_byte = Uuid::new_v4().as_bytes()[0];

    let mut raw = Vec::with_capacity(1 + meta_bytes.len() + 4 + 16 + 1);
    raw.push(random_byte);
    raw.extend(meta_bytes);
    raw.extend(relative_seconds.to_le_bytes());
    raw.extend(&digest[..16]);
    raw.push(3);

    for byte in raw.iter_mut().skip(1) {
        *byte ^= random_byte;
    }

    Some(
        general_purpose::STANDARD
            .encode(raw)
            .trim_end_matches('=')
            .to_string(),
    )
}

fn normalized_base_urls(configured_base_url: Option<&str>) -> (String, String, String) {
    let raw_base_url = configured_base_url
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            std::env::var("GROK_BASE_URL")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| "https://grok.com/rest".to_string());
    let trimmed = raw_base_url.trim_matches('/');

    if trimmed.ends_with("/rest/app-chat") {
        let root = trimmed.trim_end_matches("/app-chat").to_string();
        let web = root.trim_end_matches("/rest").to_string();
        return (trimmed.to_string(), root, web);
    }

    if trimmed.ends_with("/rest") {
        let web = trimmed.trim_end_matches("/rest").to_string();
        return (format!("{trimmed}/app-chat"), trimmed.to_string(), web);
    }

    let root = format!("{trimmed}/rest");
    (format!("{root}/app-chat"), root, trimmed.to_string())
}

fn browser_headers() -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    let values = [
        ("accept", "*/*"),
        ("accept-language", "en-US,en;q=0.9"),
        ("content-type", "application/json"),
        ("origin", "https://grok.com"),
        ("priority", "u=1, i"),
        ("referer", "https://grok.com/"),
        (
            "sec-ch-ua",
            "\"Chromium\";v=\"148\", \"Google Chrome\";v=\"148\", \"Not/A)Brand\";v=\"99\"",
        ),
        ("sec-ch-ua-arch", "\"arm\""),
        ("sec-ch-ua-bitness", "\"64\""),
        ("sec-ch-ua-full-version", "\"148.0.7778.97\""),
        (
            "sec-ch-ua-full-version-list",
            "\"Chromium\";v=\"148.0.7778.97\", \"Google Chrome\";v=\"148.0.7778.97\", \"Not/A)Brand\";v=\"99.0.0.0\"",
        ),
        ("sec-ch-ua-mobile", "?0"),
        ("sec-ch-ua-model", "\"\""),
        ("sec-ch-ua-platform", "\"macOS\""),
        ("sec-ch-ua-platform-version", "\"15.6.1\""),
        ("sec-fetch-dest", "empty"),
        ("sec-fetch-mode", "cors"),
        ("sec-fetch-site", "same-origin"),
        (
            "user-agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36",
        ),
    ];
    for (name, value) in values {
        headers.insert(name.to_string(), value.to_string());
    }
    headers
}

pub fn required_auth_cookie_names() -> BTreeSet<&'static str> {
    BTreeSet::from(["sso", "sso-rw", "x-userid", "x-anonuserid"])
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use reqwest::Method;
    use serde_json::json;

    use super::{GrokClient, RestNamespace, make_statsig_id, statsig_path};
    use crate::Result;

    fn make_client(base_url: Option<&str>) -> Result<GrokClient> {
        GrokClient::with_options(
            BTreeMap::from([("sso".to_string(), "test-cookie".to_string())]),
            false,
            base_url,
        )
    }

    #[test]
    fn normalizes_base_urls_from_rest_base() -> Result<()> {
        let client = make_client(Some("https://example.test/rest/"))?;

        assert_eq!(
            client.app_chat_base_url(),
            "https://example.test/rest/app-chat"
        );
        assert_eq!(client.root_base_url(), "https://example.test/rest");
        assert_eq!(client.web_base_url(), "https://example.test");
        Ok(())
    }

    #[test]
    fn normalizes_base_urls_from_app_chat_base() -> Result<()> {
        let client = make_client(Some("https://example.test/rest/app-chat"))?;

        assert_eq!(
            client.app_chat_base_url(),
            "https://example.test/rest/app-chat"
        );
        assert_eq!(client.root_base_url(), "https://example.test/rest");
        Ok(())
    }

    #[test]
    fn normalizes_base_urls_from_web_base() -> Result<()> {
        let client = make_client(Some("https://example.test"))?;

        assert_eq!(
            client.app_chat_base_url(),
            "https://example.test/rest/app-chat"
        );
        assert_eq!(client.root_base_url(), "https://example.test/rest");
        assert_eq!(client.web_base_url(), "https://example.test");
        Ok(())
    }

    #[test]
    fn builds_post_request_with_browser_headers_cookie_and_request_id() -> Result<()> {
        let client = GrokClient::with_options(
            BTreeMap::from([
                ("sso".to_string(), "sso-cookie".to_string()),
                ("x-anonuserid".to_string(), "anon-cookie".to_string()),
            ]),
            false,
            Some("https://example.test/rest"),
        )?;

        let request = client.make_request(
            "/conversations/new",
            Method::POST,
            Some(json!({"message": "hello"})),
            RestNamespace::AppChat,
        )?;

        assert_eq!(
            request.url,
            "https://example.test/rest/app-chat/conversations/new"
        );
        assert_eq!(
            request.headers.get("accept").map(String::as_str),
            Some("*/*")
        );
        assert_eq!(
            request.headers.get("content-type").map(String::as_str),
            Some("application/json")
        );
        assert_eq!(
            request.headers.get("origin").map(String::as_str),
            Some("https://grok.com")
        );
        assert_eq!(
            request.headers.get("Cookie").map(String::as_str),
            Some("sso=sso-cookie; x-anonuserid=anon-cookie")
        );
        assert!(
            request
                .headers
                .get("x-xai-request-id")
                .is_some_and(|value| value == &value.to_lowercase())
        );
        assert!(
            request
                .headers
                .get("x-statsig-id")
                .is_some_and(|value| !value.is_empty() && !value.contains('='))
        );
        Ok(())
    }

    #[test]
    fn creates_client_from_json_file() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let credentials_path = temp_dir.path().join("credentials.json");
        std::fs::write(
            &credentials_path,
            r#"{"x-anonuserid":"anon-cookie","sso":"sso-cookie"}"#,
        )?;

        let client =
            GrokClient::from_json_file(&credentials_path, true, Some("https://example.test/rest"))?;

        assert!(client.is_debug);
        assert_eq!(client.root_base_url(), "https://example.test/rest");
        assert_eq!(
            client.cookie_header(),
            "sso=sso-cookie; x-anonuserid=anon-cookie"
        );
        Ok(())
    }

    #[test]
    fn get_request_without_payload_omits_content_type_origin_and_body() -> Result<()> {
        let client = make_client(Some("https://example.test/rest"))?;
        let request =
            client.make_request("/subscriptions", Method::GET, None, RestNamespace::Root)?;

        assert_eq!(request.method, Method::GET);
        assert_eq!(request.url, "https://example.test/rest/subscriptions");
        assert!(!request.headers.contains_key("content-type"));
        assert!(!request.headers.contains_key("origin"));
        assert!(request.body.is_none());
        assert_eq!(
            request.headers.get("Cookie").map(String::as_str),
            Some("sso=test-cookie")
        );
        assert!(request.headers.contains_key("x-xai-request-id"));
        assert!(request.headers.contains_key("x-statsig-id"));
        Ok(())
    }

    #[test]
    fn curl_representation_redacts_cookies_when_requested() -> Result<()> {
        let client = GrokClient::with_options(
            BTreeMap::from([
                ("sso".to_string(), "secret-cookie".to_string()),
                ("x-anonuserid".to_string(), "secret-anon".to_string()),
            ]),
            false,
            Some("https://example.test/rest"),
        )?;
        let request = client.make_request(
            "/conversations/new",
            Method::POST,
            Some(json!({"message": "hello"})),
            RestNamespace::AppChat,
        )?;

        let curl = request.curl_representation(true);

        assert!(curl.contains("Cookie: <redacted>"));
        assert!(!curl.contains("secret-cookie"));
        assert!(!curl.contains("secret-anon"));
        assert!(curl.contains("\"https://example.test/rest/app-chat/conversations/new\""));
        Ok(())
    }

    #[test]
    fn curl_representation_redacts_sensitive_json_payloads_like_swift() -> Result<()> {
        let client = make_client(Some("https://example.test/rest"))?;
        let request = client.make_request(
            "/speech-to-text",
            Method::POST,
            Some(json!({
                "audioBase64": "YWJjZGVmZw==",
                "audioFormat": "webm",
                "nested": {
                    "data": "secret",
                    "content": ["hidden"]
                },
                "file": {
                    "name": "clip.webm"
                }
            })),
            RestNamespace::Root,
        )?;

        let curl = request.curl_representation(false);

        assert!(!curl.contains("YWJjZGVmZw=="));
        assert!(!curl.contains("secret"));
        assert!(!curl.contains("hidden"));
        assert!(curl.contains("<redacted"));
        assert!(curl.contains(r#""audioFormat":"webm""#));
        Ok(())
    }

    #[test]
    fn statsig_path_uses_namespace_prefix_and_omits_query() {
        assert_eq!(
            statsig_path("/share_links?pageSize=9", RestNamespace::AppChat),
            "/rest/app-chat/share_links"
        );
        assert_eq!(
            statsig_path("/subscriptions?x=1", RestNamespace::Root),
            "/rest/subscriptions"
        );
    }

    #[test]
    fn statsig_id_is_nonempty_unpadded_base64() {
        let id = make_statsig_id("/conversations/new", &Method::POST, RestNamespace::AppChat);
        assert!(id.is_some_and(|value| !value.is_empty() && !value.contains('=')));
    }
}
