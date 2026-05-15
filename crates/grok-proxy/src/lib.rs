use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, Request as AxumRequest, State};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64_STANDARD};
use futures_util::stream;
use grok_client::{
    ConversationResponse, DEFAULT_SPEECH_REFINEMENT_LEVEL, GrokClient, GrokMessageOptions,
    GrokMode, GrokSpeechToTextOptions,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tower_http::cors::{Any, CorsLayer};
use uuid::Uuid;

pub type ProxyFuture<T> = Pin<Box<dyn Future<Output = Result<T, ProxyError>> + Send>>;

pub const DEFAULT_PROXY_HOSTNAME: &str = "127.0.0.1";
pub const DEFAULT_PROXY_PORT: u16 = 8080;
pub const DEFAULT_PROXY_MAX_BODY_SIZE: usize = 50 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Backend(String),
}

impl From<grok_client::GrokError> for ProxyError {
    fn from(error: grok_client::GrokError) -> Self {
        Self::Backend(error.to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProxyCredentialsSource {
    Environment,
    File(PathBuf),
    Mock,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProxyCredentials {
    pub cookies: BTreeMap<String, String>,
    pub source: ProxyCredentialsSource,
}

#[derive(Debug, thiserror::Error)]
pub enum ProxyConfigurationError {
    #[error("Invalid credentials file {path}: {reason}")]
    InvalidCredentialsFile { path: PathBuf, reason: String },
    #[error("Could not read credentials file {path}: {source}")]
    FileReadError {
        path: PathBuf,
        source: std::io::Error,
    },
}

pub fn load_proxy_credentials_from_environment() -> Result<ProxyCredentials, ProxyConfigurationError>
{
    load_proxy_credentials(
        |name| std::env::var(name).ok(),
        Path::new("credentials.json"),
    )
}

pub fn load_proxy_credentials<F>(
    env: F,
    credentials_path: &Path,
) -> Result<ProxyCredentials, ProxyConfigurationError>
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(raw_cookies) = env("GROK_COOKIES")
        && let Some(cookies) = parse_proxy_cookies(&raw_cookies)
    {
        return Ok(ProxyCredentials {
            cookies,
            source: ProxyCredentialsSource::Environment,
        });
    }

    if credentials_path.exists() {
        let data = fs::read(credentials_path).map_err(|source| {
            ProxyConfigurationError::FileReadError {
                path: credentials_path.to_path_buf(),
                source,
            }
        })?;
        let cookies = serde_json::from_slice::<BTreeMap<String, String>>(&data)
            .ok()
            .filter(|cookies| !cookies.is_empty())
            .ok_or_else(|| ProxyConfigurationError::InvalidCredentialsFile {
                path: credentials_path.to_path_buf(),
                reason: "expected a non-empty JSON object whose keys and values are strings"
                    .to_string(),
            })?;
        return Ok(ProxyCredentials {
            cookies,
            source: ProxyCredentialsSource::File(credentials_path.to_path_buf()),
        });
    }

    Ok(ProxyCredentials {
        cookies: mock_proxy_cookies(),
        source: ProxyCredentialsSource::Mock,
    })
}

fn parse_proxy_cookies(raw_cookies: &str) -> Option<BTreeMap<String, String>> {
    serde_json::from_str::<BTreeMap<String, String>>(raw_cookies)
        .ok()
        .filter(|cookies| !cookies.is_empty())
}

pub fn mock_proxy_cookies() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("sso".to_string(), "mock-sso".to_string()),
        ("sso-rw".to_string(), "mock-sso-rw".to_string()),
        ("x-anonuserid".to_string(), "mock-user-id".to_string()),
        ("x-challenge".to_string(), "mock-challenge".to_string()),
        ("x-signature".to_string(), "mock-signature".to_string()),
    ])
}

pub fn configured_proxy_max_body_size<F>(env: F) -> usize
where
    F: Fn(&str) -> Option<String>,
{
    env("GROK_PROXY_MAX_BODY_SIZE")
        .as_deref()
        .and_then(parse_proxy_byte_count)
        .unwrap_or(DEFAULT_PROXY_MAX_BODY_SIZE)
}

pub fn parse_proxy_byte_count(raw_value: &str) -> Option<usize> {
    let value = raw_value
        .to_lowercase()
        .trim()
        .replace(char::is_whitespace, "");
    let suffixes = [
        ("kb", 1024usize),
        ("mb", 1024usize * 1024),
        ("gb", 1024usize * 1024 * 1024),
        ("b", 1usize),
    ];

    for (suffix, multiplier) in suffixes {
        if let Some(number_part) = value.strip_suffix(suffix) {
            let amount = number_part.parse::<usize>().ok()?;
            return amount.checked_mul(multiplier);
        }
    }

    value.parse::<usize>().ok()
}

pub trait GrokProxyBackend: Send + Sync + 'static {
    fn list_modes(&self) -> ProxyFuture<Vec<GrokMode>>;

    fn send_message(
        &self,
        message: String,
        options: GrokMessageOptions,
    ) -> ProxyFuture<ConversationResponse>;

    fn stream_message(
        &self,
        message: String,
        options: GrokMessageOptions,
        sender: mpsc::UnboundedSender<Result<ConversationResponse, ProxyError>>,
    ) -> ProxyFuture<()>;

    fn transcribe_audio(&self, request: AudioTranscriptionRequest) -> ProxyFuture<String>;
}

#[derive(Clone)]
pub struct GrokClientBackend {
    client: Arc<GrokClient>,
}

impl GrokClientBackend {
    pub fn new(client: GrokClient) -> Self {
        Self {
            client: Arc::new(client),
        }
    }
}

impl GrokProxyBackend for GrokClientBackend {
    fn list_modes(&self) -> ProxyFuture<Vec<GrokMode>> {
        let client = self.client.clone();
        Box::pin(async move { Ok(client.list_modes().await?) })
    }

    fn send_message(
        &self,
        message: String,
        options: GrokMessageOptions,
    ) -> ProxyFuture<ConversationResponse> {
        let client = self.client.clone();
        Box::pin(async move { Ok(client.send_message_response(&message, &options).await?) })
    }

    fn stream_message(
        &self,
        message: String,
        options: GrokMessageOptions,
        sender: mpsc::UnboundedSender<Result<ConversationResponse, ProxyError>>,
    ) -> ProxyFuture<()> {
        let client = self.client.clone();
        Box::pin(async move {
            client
                .stream_message_responses(&message, &options, |response| {
                    sender.send(Ok(response)).map_err(|_| {
                        grok_client::GrokError::Streaming("stream receiver closed".to_string())
                    })?;
                    Ok(())
                })
                .await?;
            Ok(())
        })
    }

    fn transcribe_audio(&self, request: AudioTranscriptionRequest) -> ProxyFuture<String> {
        let client = self.client.clone();
        Box::pin(async move {
            let audio_format = request
                .audio_format
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    ProxyError::BadRequest(
                        "audio_format is required when the audio format cannot be inferred"
                            .to_string(),
                    )
                })?
                .to_string();
            let refinement_level = request
                .refinement_level
                .as_deref()
                .unwrap_or(DEFAULT_SPEECH_REFINEMENT_LEVEL)
                .to_string();
            let response = client
                .speech_to_text_response(
                    &request.audio_base64,
                    &GrokSpeechToTextOptions {
                        audio_format: Some(audio_format),
                        refinement_level,
                    },
                )
                .await?;
            Ok(response.text)
        })
    }
}

#[derive(Clone)]
pub struct ProxyState {
    backend: Arc<dyn GrokProxyBackend>,
}

impl ProxyState {
    pub fn new(backend: Arc<dyn GrokProxyBackend>) -> Self {
        Self { backend }
    }
}

pub fn router(backend: Arc<dyn GrokProxyBackend>) -> Router {
    router_with_max_body_size(backend, DEFAULT_PROXY_MAX_BODY_SIZE)
}

pub fn router_with_max_body_size(
    backend: Arc<dyn GrokProxyBackend>,
    max_body_size: usize,
) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/hello", get(hello))
        .route("/v1/models", get(models))
        .route("/models", get(models))
        .route("/v1/chat/completions", post(chat_completions))
        .route(
            "/v1/audio/transcriptions",
            post(audio_transcriptions).layer(DefaultBodyLimit::max(max_body_size)),
        )
        .layer(proxy_cors_layer())
        .with_state(ProxyState::new(backend))
}

pub fn router_for_client(client: GrokClient) -> Router {
    router(Arc::new(GrokClientBackend::new(client)))
}

pub fn proxy_cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            header::ACCEPT,
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::ORIGIN,
            header::USER_AGENT,
            HeaderName::from_static("access-control-allow-origin"),
            HeaderName::from_static("x-requested-with"),
        ])
}

async fn root() -> &'static str {
    "GrokProxy: OpenAI-compatible proxy for Grok"
}

async fn hello() -> &'static str {
    "Hello, world!"
}

async fn models(
    State(state): State<ProxyState>,
) -> Result<Json<ModelsResponse>, (StatusCode, String)> {
    let modes = state.backend.list_modes().await.map_err(|error| {
        (
            StatusCode::BAD_GATEWAY,
            format!("Failed to fetch Grok modes: {error}"),
        )
    })?;
    Ok(Json(ModelsResponse::response_for(&modes)))
}

async fn chat_completions(
    State(state): State<ProxyState>,
    Json(request): Json<ChatCompletionRequest>,
) -> Result<Response, (StatusCode, String)> {
    validate_chat_request(&request)?;

    let user_message = last_user_message(&request)?;
    let options = GrokMessageOptions {
        temporary: true,
        mode_id: GrokMode::resolve(Some(&request.model)).id,
        ..GrokMessageOptions::default()
    };

    if request.stream.unwrap_or(false) {
        return Ok(streaming_chat_response(
            state.backend,
            request.model,
            user_message,
            options,
        ));
    }

    let response = state
        .backend
        .send_message(user_message, options)
        .await
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to communicate with Grok: {error}"),
            )
        })?;

    Ok(Json(ChatCompletionResponse::create(
        &request.model,
        &response.message,
    ))
    .into_response())
}

async fn audio_transcriptions(
    State(state): State<ProxyState>,
    request: AxumRequest,
) -> Result<Response, (StatusCode, String)> {
    let request = decode_audio_transcription_request(request).await?;
    match request.response_format.as_str() {
        "json" => {
            let text = state
                .backend
                .transcribe_audio(request)
                .await
                .map_err(map_audio_backend_error)?;
            Ok(Json(AudioTranscriptionResponse { text }).into_response())
        }
        "text" => {
            let text = state
                .backend
                .transcribe_audio(request)
                .await
                .map_err(map_audio_backend_error)?;
            let mut response = text.into_response();
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            );
            Ok(response)
        }
        response_format => Err((
            StatusCode::BAD_REQUEST,
            format!("Unsupported response_format: {response_format}"),
        )),
    }
}

fn map_audio_backend_error(error: ProxyError) -> (StatusCode, String) {
    match error {
        ProxyError::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
        ProxyError::Backend(message) => (
            StatusCode::BAD_GATEWAY,
            format!("Failed to communicate with Grok: {message}"),
        ),
    }
}

fn streaming_chat_response(
    backend: Arc<dyn GrokProxyBackend>,
    model: String,
    user_message: String,
    options: GrokMessageOptions,
) -> Response {
    let (response_sender, mut response_receiver) =
        mpsc::unbounded_channel::<Result<ConversationResponse, ProxyError>>();
    let error_sender = response_sender.clone();
    tokio::spawn(async move {
        if let Err(error) = backend
            .stream_message(user_message, options, response_sender)
            .await
        {
            let _ = error_sender.send(Err(error));
        }
    });

    let (event_sender, event_receiver) = mpsc::unbounded_channel::<Bytes>();
    tokio::spawn(async move {
        let response_id = Uuid::new_v4().hyphenated().to_string();
        let mut is_first_chunk = true;
        let mut emitted_content = false;
        let mut emitted_final_chunk = false;

        while let Some(item) = response_receiver.recv().await {
            match item {
                Ok(response) => {
                    let chunks = streaming_chunks(
                        &response,
                        &model,
                        &response_id,
                        &mut is_first_chunk,
                        &mut emitted_content,
                    );
                    if response.is_final {
                        emitted_final_chunk = true;
                    }
                    for chunk in chunks {
                        if !send_server_sent_event(&event_sender, &chunk) {
                            return;
                        }
                    }
                }
                Err(error) => {
                    let _ = event_sender.send(Bytes::from(format!(
                        "data: {{\"error\": {}}}\n\n",
                        serde_json::to_string(&error.to_string())
                            .unwrap_or_else(|_| "\"stream error\"".to_string())
                    )));
                    return;
                }
            }
        }

        if !emitted_final_chunk {
            let done_chunk = ChatCompletionChunkResponse::create_done_chunk(&response_id, &model);
            if !send_server_sent_event(&event_sender, &done_chunk) {
                return;
            }
        }
        let _ = event_sender.send(Bytes::from(done_server_sent_event()));
    });

    let body_stream = stream::unfold(event_receiver, |mut receiver| async {
        receiver
            .recv()
            .await
            .map(|bytes| (Ok::<Bytes, Infallible>(bytes), receiver))
    });
    let mut response = Response::new(Body::from_stream(body_stream));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
}

fn validate_chat_request(request: &ChatCompletionRequest) -> Result<(), (StatusCode, String)> {
    if request.messages.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Messages array must not be empty".to_string(),
        ));
    }

    for message in &request.messages {
        if !ChatCompletionMessage::is_valid_role(&message.role) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Invalid role: {}", message.role),
            ));
        }
    }

    last_user_message(request).map(|_| ())
}

fn last_user_message(request: &ChatCompletionRequest) -> Result<String, (StatusCode, String)> {
    request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .map(|message| message.content.clone())
        .filter(|content| !content.is_empty())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "At least one user message is required".to_string(),
            )
        })
}

async fn decode_audio_transcription_request(
    request: AxumRequest,
) -> Result<AudioTranscriptionRequest, (StatusCode, String)> {
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();

    if is_json_content_type(&content_type) {
        let body = axum::body::to_bytes(request.into_body(), usize::MAX)
            .await
            .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
        return decode_audio_transcription_json(&body);
    }

    let mut multipart = Multipart::from_request(request, &())
        .await
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    decode_audio_transcription_multipart(&mut multipart).await
}

fn is_json_content_type(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .map(str::trim)
        .is_some_and(|media_type| media_type.eq_ignore_ascii_case("application/json"))
}

fn decode_audio_transcription_json(
    body: &Bytes,
) -> Result<AudioTranscriptionRequest, (StatusCode, String)> {
    let request: AudioTranscriptionJsonRequest = serde_json::from_slice(body)
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    let audio_base64 = request
        .audio_base64_camel
        .or(request.audio_base64)
        .or(request.content)
        .or(request.data)
        .or(request.file)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "JSON transcription requests require file, audioBase64, content, or data"
                    .to_string(),
            )
        })?;
    let trimmed_audio_base64 = audio_base64.trim().to_string();
    if BASE64_STANDARD.decode(&trimmed_audio_base64).is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            "JSON transcription audio must be raw base64".to_string(),
        ));
    }

    Ok(AudioTranscriptionRequest {
        model: request.model,
        audio_base64: trimmed_audio_base64,
        response_format: normalize_response_format(request.response_format),
        audio_format: request.audio_format_camel.or(request.audio_format),
        refinement_level: request.refinement_level_camel.or(request.refinement_level),
        language: request.language,
        prompt: request.prompt,
    })
}

async fn decode_audio_transcription_multipart(
    multipart: &mut Multipart,
) -> Result<AudioTranscriptionRequest, (StatusCode, String)> {
    let mut fields = HashMap::new();
    let mut file_bytes = None;
    let mut file_name = None;
    let mut file_content_type = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?
    {
        let name = field.name().map(str::to_string).ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "Multipart transcription part is missing field name".to_string(),
            )
        })?;
        let uploaded_file_name = field.file_name().map(str::to_string);
        let uploaded_content_type = field.content_type().map(str::to_string);
        let body = field
            .bytes()
            .await
            .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;

        if name == "file" {
            file_bytes = Some(body.to_vec());
            file_name = uploaded_file_name;
            file_content_type = uploaded_content_type;
        } else {
            fields.insert(name, String::from_utf8_lossy(&body).to_string());
        }
    }

    let audio_bytes = file_bytes
        .filter(|bytes| !bytes.is_empty())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "Multipart transcription requests require a non-empty file".to_string(),
            )
        })?;
    let model = fields
        .remove("model")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "Multipart transcription requests require model".to_string(),
            )
        })?;
    let audio_format = fields.remove("audio_format").or_else(|| {
        infer_multipart_audio_format(file_name.as_deref(), file_content_type.as_deref())
    });

    Ok(AudioTranscriptionRequest {
        model,
        audio_base64: BASE64_STANDARD.encode(audio_bytes),
        response_format: normalize_response_format(fields.remove("response_format")),
        audio_format,
        refinement_level: fields.remove("refinement_level"),
        language: fields.remove("language"),
        prompt: fields.remove("prompt"),
    })
}

fn normalize_response_format(response_format: Option<String>) -> String {
    response_format
        .map(|value| value.to_lowercase())
        .unwrap_or_else(|| "json".to_string())
}

fn infer_multipart_audio_format(
    file_name: Option<&str>,
    content_type: Option<&str>,
) -> Option<String> {
    if let Some(extension) = file_name
        .and_then(|value| {
            value
                .rsplit_once('.')
                .map(|(_, extension)| extension.trim())
        })
        .filter(|extension| !extension.is_empty())
    {
        return Some(extension.to_lowercase());
    }

    content_type
        .and_then(|value| value.split(';').next())
        .and_then(|media_type| {
            media_type
                .rsplit_once('/')
                .map(|(_, subtype)| subtype.trim())
        })
        .filter(|subtype| !subtype.is_empty())
        .map(str::to_lowercase)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioTranscriptionRequest {
    pub model: String,
    pub audio_base64: String,
    pub response_format: String,
    pub audio_format: Option<String>,
    pub refinement_level: Option<String>,
    pub language: Option<String>,
    pub prompt: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AudioTranscriptionResponse {
    pub text: String,
}

#[derive(Debug, Deserialize)]
struct AudioTranscriptionJsonRequest {
    model: String,
    file: Option<String>,
    #[serde(rename = "audioBase64")]
    audio_base64_camel: Option<String>,
    audio_base64: Option<String>,
    content: Option<String>,
    data: Option<String>,
    response_format: Option<String>,
    #[serde(rename = "audioFormat")]
    audio_format_camel: Option<String>,
    audio_format: Option<String>,
    #[serde(rename = "refinementLevel")]
    refinement_level_camel: Option<String>,
    refinement_level: Option<String>,
    language: Option<String>,
    prompt: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatCompletionMessage>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<i64>,
    pub top_p: Option<f64>,
    pub frequency_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub stream: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ChatCompletionMessage {
    pub role: String,
    pub content: String,
}

impl ChatCompletionMessage {
    fn is_valid_role(role: &str) -> bool {
        matches!(role, "system" | "user" | "assistant")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatCompletionChoice>,
    pub usage: ChatCompletionUsage,
    pub service_tier: String,
}

impl ChatCompletionResponse {
    pub fn create(model: &str, message: &str) -> Self {
        Self {
            id: Uuid::new_v4().hyphenated().to_string(),
            object: "chat.completion".to_string(),
            created: unix_timestamp(),
            model: model.to_string(),
            choices: vec![ChatCompletionChoice {
                index: 0,
                message: ChatCompletionChoiceMessage {
                    role: "assistant".to_string(),
                    content: message.to_string(),
                    refusal: None,
                    annotations: Vec::new(),
                },
                logprobs: None,
                finish_reason: "stop".to_string(),
            }],
            usage: ChatCompletionUsage::default(),
            service_tier: "default".to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatCompletionChunkResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub system_fingerprint: String,
    pub choices: Vec<ChatCompletionChunkChoice>,
}

impl ChatCompletionChunkResponse {
    pub fn create(id: &str, model: &str, chunk: &str, include_role: bool) -> Self {
        Self {
            id: id.to_string(),
            object: "chat.completion.chunk".to_string(),
            created: unix_timestamp(),
            model: model.to_string(),
            system_fingerprint: system_fingerprint(),
            choices: vec![ChatCompletionChunkChoice {
                index: 0,
                delta: ChatCompletionChunkDelta {
                    role: include_role.then(|| "assistant".to_string()),
                    content: Some(chunk.to_string()),
                },
                logprobs: None,
                finish_reason: None,
            }],
        }
    }

    pub fn create_done_chunk(id: &str, model: &str) -> Self {
        Self {
            id: id.to_string(),
            object: "chat.completion.chunk".to_string(),
            created: unix_timestamp(),
            model: model.to_string(),
            system_fingerprint: system_fingerprint(),
            choices: vec![ChatCompletionChunkChoice {
                index: 0,
                delta: ChatCompletionChunkDelta {
                    role: None,
                    content: None,
                },
                logprobs: None,
                finish_reason: Some("stop".to_string()),
            }],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatCompletionChunkChoice {
    pub index: i64,
    pub delta: ChatCompletionChunkDelta,
    pub logprobs: Option<String>,
    pub finish_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatCompletionChunkDelta {
    pub role: Option<String>,
    pub content: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatCompletionChoice {
    pub index: i64,
    pub message: ChatCompletionChoiceMessage,
    pub logprobs: Option<String>,
    pub finish_reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatCompletionChoiceMessage {
    pub role: String,
    pub content: String,
    pub refusal: Option<String>,
    pub annotations: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ChatCompletionUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub prompt_tokens_details: TokenDetails,
    pub completion_tokens_details: CompletionTokenDetails,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct TokenDetails {
    pub cached_tokens: i64,
    pub audio_tokens: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct CompletionTokenDetails {
    pub reasoning_tokens: i64,
    pub audio_tokens: i64,
    pub accepted_prediction_tokens: i64,
    pub rejected_prediction_tokens: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelsResponse {
    pub object: String,
    pub data: Vec<ModelResponse>,
}

impl ModelsResponse {
    pub fn response_for(modes: &[GrokMode]) -> Self {
        Self {
            object: "list".to_string(),
            data: modes.iter().map(ModelResponse::from_mode).collect(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub owned_by: String,
    pub available: bool,
    pub disabled: bool,
    pub unavailable_reason: Option<String>,
    pub minimum_subscription_tier: Option<String>,
}

impl ModelResponse {
    fn from_mode(mode: &GrokMode) -> Self {
        Self {
            id: mode.id.clone(),
            object: "model".to_string(),
            created: unix_timestamp() - 86_400,
            owned_by: "grok".to_string(),
            available: mode.is_available,
            disabled: !mode.is_available,
            unavailable_reason: mode.unavailable_reason.clone(),
            minimum_subscription_tier: mode.minimum_subscription_tier.clone(),
        }
    }
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn system_fingerprint() -> String {
    format!(
        "fp_{}",
        Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(12)
            .collect::<String>()
    )
}

pub fn done_server_sent_event() -> &'static str {
    "data: [DONE]\n\n"
}

pub fn server_sent_event(chunk: &ChatCompletionChunkResponse) -> Result<String, serde_json::Error> {
    Ok(format!("data: {}\n\n", serde_json::to_string(chunk)?))
}

fn send_server_sent_event(
    sender: &mpsc::UnboundedSender<Bytes>,
    chunk: &ChatCompletionChunkResponse,
) -> bool {
    match server_sent_event(chunk) {
        Ok(event) => sender.send(Bytes::from(event)).is_ok(),
        Err(error) => sender
            .send(Bytes::from(format!(
                "data: {{\"error\": {}}}\n\n",
                serde_json::to_string(&error.to_string())
                    .unwrap_or_else(|_| "\"stream serialization error\"".to_string())
            )))
            .is_ok(),
    }
}

pub fn terminal_server_sent_events(
    emitted_final_chunk: bool,
    model: &str,
    response_id: &str,
) -> Result<Vec<String>, serde_json::Error> {
    let mut events = Vec::new();
    if !emitted_final_chunk {
        events.push(server_sent_event(
            &ChatCompletionChunkResponse::create_done_chunk(response_id, model),
        )?);
    }
    events.push(done_server_sent_event().to_string());
    Ok(events)
}

pub fn streaming_chunks(
    response: &ConversationResponse,
    model: &str,
    response_id: &str,
    is_first_chunk: &mut bool,
    emitted_content: &mut bool,
) -> Vec<ChatCompletionChunkResponse> {
    if response.is_thinking && !response.is_final {
        return Vec::new();
    }

    if response.is_final {
        let mut chunks = Vec::new();
        if !*emitted_content && !response.message.is_empty() {
            chunks.push(ChatCompletionChunkResponse::create(
                response_id,
                model,
                &response.message,
                *is_first_chunk,
            ));
            *is_first_chunk = false;
            *emitted_content = true;
        }
        chunks.push(ChatCompletionChunkResponse::create_done_chunk(
            response_id,
            model,
        ));
        return chunks;
    }

    let chunk =
        ChatCompletionChunkResponse::create(response_id, model, &response.message, *is_first_chunk);
    *is_first_chunk = false;
    if !response.message.is_empty() {
        *emitted_content = true;
    }
    vec![chunk]
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use base64::Engine;
    use serde_json::{Value, json};
    use std::fs;
    use std::sync::Mutex;
    use tempfile::tempdir;
    use tower::ServiceExt;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    #[derive(Clone)]
    struct MockBackend {
        modes: Vec<GrokMode>,
        response: ConversationResponse,
        stream_responses: Vec<ConversationResponse>,
        stream_error: Option<String>,
        transcription_text: String,
        transcriptions: Arc<Mutex<Vec<AudioTranscriptionRequest>>>,
    }

    impl GrokProxyBackend for MockBackend {
        fn list_modes(&self) -> ProxyFuture<Vec<GrokMode>> {
            let modes = self.modes.clone();
            Box::pin(async move { Ok(modes) })
        }

        fn send_message(
            &self,
            message: String,
            options: GrokMessageOptions,
        ) -> ProxyFuture<ConversationResponse> {
            let response = self.response.clone();
            Box::pin(async move {
                assert_eq!(message, "Use the final user message");
                assert!(options.temporary);
                assert_eq!(options.mode_id, "fast");
                Ok(response)
            })
        }

        fn stream_message(
            &self,
            message: String,
            options: GrokMessageOptions,
            sender: mpsc::UnboundedSender<Result<ConversationResponse, ProxyError>>,
        ) -> ProxyFuture<()> {
            let stream_responses = self.stream_responses.clone();
            let stream_error = self.stream_error.clone();
            Box::pin(async move {
                assert_eq!(message, "Use the final user message");
                assert!(options.temporary);
                assert_eq!(options.mode_id, "fast");
                for response in stream_responses {
                    if sender.send(Ok(response)).is_err() {
                        return Ok(());
                    }
                }
                if let Some(message) = stream_error {
                    return Err(ProxyError::Backend(message));
                }
                Ok(())
            })
        }

        fn transcribe_audio(&self, request: AudioTranscriptionRequest) -> ProxyFuture<String> {
            let transcription_text = self.transcription_text.clone();
            let transcriptions = self.transcriptions.clone();
            Box::pin(async move {
                {
                    let mut guard = transcriptions.lock().map_err(|_| {
                        ProxyError::Backend("transcription recorder lock poisoned".to_string())
                    })?;
                    guard.push(request);
                }
                Ok(transcription_text)
            })
        }
    }

    fn mock_backend() -> Arc<MockBackend> {
        Arc::new(MockBackend {
            modes: vec![
                GrokMode::with_summary("fast", "Fast", "Quick responses"),
                GrokMode {
                    id: "heavy".to_string(),
                    display_name: "Heavy".to_string(),
                    summary: "Team of Experts".to_string(),
                    is_available: false,
                    unavailable_reason: None,
                    minimum_subscription_tier: Some("TIER_SUPERGROK_HEAVY".to_string()),
                },
            ],
            response: ConversationResponse::final_message(
                "Proxy answer".to_string(),
                "conv-1".to_string(),
                "resp-1".to_string(),
                None,
                None,
                false,
            ),
            stream_responses: vec![
                ConversationResponse {
                    message: "Thinking".to_string(),
                    conversation_id: "conv-1".to_string(),
                    response_id: "resp-1".to_string(),
                    timestamp: None,
                    web_search_results: None,
                    xposts: None,
                    is_thinking: true,
                    is_soft_stop: false,
                    is_final: false,
                },
                ConversationResponse {
                    message: "Streaming answer".to_string(),
                    conversation_id: "conv-1".to_string(),
                    response_id: "resp-1".to_string(),
                    timestamp: None,
                    web_search_results: None,
                    xposts: None,
                    is_thinking: false,
                    is_soft_stop: false,
                    is_final: true,
                },
            ],
            stream_error: None,
            transcription_text: "hello audio".to_string(),
            transcriptions: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn test_app() -> Router {
        router(mock_backend())
    }

    fn test_app_with_backend() -> (Router, Arc<MockBackend>) {
        let backend = mock_backend();
        let routed_backend: Arc<dyn GrokProxyBackend> = backend.clone();
        (router(routed_backend), backend)
    }

    fn recorded_transcriptions(
        backend: &MockBackend,
    ) -> TestResult<Vec<AudioTranscriptionRequest>> {
        let guard = backend
            .transcriptions
            .lock()
            .map_err(|_| std::io::Error::other("transcription recorder lock poisoned"))?;
        Ok(guard.clone())
    }

    async fn body_json(response: axum::response::Response) -> TestResult<Value> {
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    async fn body_text(response: axum::response::Response) -> TestResult<String> {
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        Ok(String::from_utf8(bytes.to_vec())?)
    }

    #[tokio::test]
    async fn health_routes_match_swift_proxy() -> TestResult {
        let app = test_app();
        let response = app
            .clone()
            .oneshot(Request::get("/").body(Body::empty())?)
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body_text(response).await?,
            "GrokProxy: OpenAI-compatible proxy for Grok"
        );

        let response = app
            .oneshot(Request::get("/hello").body(Body::empty())?)
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_text(response).await?, "Hello, world!");
        Ok(())
    }

    #[tokio::test]
    async fn models_endpoints_return_openai_list_shape() -> TestResult {
        for path in ["/v1/models", "/models"] {
            let response = test_app()
                .oneshot(Request::get(path).body(Body::empty())?)
                .await?;
            assert_eq!(response.status(), StatusCode::OK);
            let json = body_json(response).await?;
            assert_eq!(json["object"], "list");
            assert_eq!(json["data"][0]["id"], "fast");
            assert_eq!(json["data"][0]["object"], "model");
            assert_eq!(json["data"][0]["owned_by"], "grok");
            assert_eq!(json["data"][0]["available"], true);
            assert_eq!(json["data"][0]["disabled"], false);
            assert_eq!(json["data"][1]["id"], "heavy");
            assert_eq!(json["data"][1]["available"], false);
            assert_eq!(json["data"][1]["disabled"], true);
            assert_eq!(
                json["data"][1]["minimum_subscription_tier"],
                "TIER_SUPERGROK_HEAVY"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn cors_preflight_matches_swift_proxy_configuration() -> TestResult {
        let response = test_app()
            .oneshot(
                Request::options("/v1/chat/completions")
                    .header("origin", "http://localhost:3000")
                    .header("access-control-request-method", "POST")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .and_then(|value| value.to_str().ok()),
            Some("*")
        );
        assert!(
            response
                .headers()
                .get("access-control-allow-methods")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|methods| methods.contains("POST"))
        );
        Ok(())
    }

    #[test]
    fn proxy_byte_count_parser_matches_swift_configuration_suffixes() {
        assert_eq!(
            configured_proxy_max_body_size(|_| None),
            DEFAULT_PROXY_MAX_BODY_SIZE
        );
        assert_eq!(parse_proxy_byte_count("12"), Some(12));
        assert_eq!(parse_proxy_byte_count("12b"), Some(12));
        assert_eq!(parse_proxy_byte_count("3 kb"), Some(3 * 1024));
        assert_eq!(parse_proxy_byte_count("2MB"), Some(2 * 1024 * 1024));
        assert_eq!(parse_proxy_byte_count("1gb"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_proxy_byte_count("-1"), None);
        assert_eq!(parse_proxy_byte_count("nope"), None);
    }

    #[test]
    fn proxy_credentials_prefer_environment_then_file_then_mock_like_swift() -> TestResult {
        let temp_dir = tempdir()?;
        let credentials_path = temp_dir.path().join("credentials.json");
        fs::write(&credentials_path, br#"{"sso":"from-file"}"#)?;

        let credentials = load_proxy_credentials(
            |name| (name == "GROK_COOKIES").then(|| r#"{"sso":"from-env"}"#.to_string()),
            &credentials_path,
        )?;
        assert_eq!(credentials.source, ProxyCredentialsSource::Environment);
        assert_eq!(
            credentials.cookies.get("sso").map(String::as_str),
            Some("from-env")
        );

        let credentials = load_proxy_credentials(
            |name| (name == "GROK_COOKIES").then(|| "not json".to_string()),
            &credentials_path,
        )?;
        assert_eq!(
            credentials.source,
            ProxyCredentialsSource::File(credentials_path.clone())
        );
        assert_eq!(
            credentials.cookies.get("sso").map(String::as_str),
            Some("from-file")
        );

        let missing_path = temp_dir.path().join("missing.json");
        let credentials = load_proxy_credentials(|_| None, &missing_path)?;
        assert_eq!(credentials.source, ProxyCredentialsSource::Mock);
        assert_eq!(
            credentials.cookies.get("x-anonuserid").map(String::as_str),
            Some("mock-user-id")
        );
        Ok(())
    }

    #[test]
    fn proxy_credentials_reject_invalid_existing_credentials_file() -> TestResult {
        let temp_dir = tempdir()?;
        let credentials_path = temp_dir.path().join("credentials.json");
        fs::write(&credentials_path, br#"[]"#)?;

        let Err(error) = load_proxy_credentials(|_| None, &credentials_path) else {
            panic!("invalid credentials file should fail");
        };
        assert!(error.to_string().contains("Invalid credentials file"));
        Ok(())
    }

    #[test]
    fn proxy_credentials_malformed_json_preserves_invalid_credentials_file_contract() -> TestResult
    {
        let temp_dir = tempdir()?;
        let credentials_path = temp_dir.path().join("credentials.json");
        fs::write(&credentials_path, br#"{"sso":"unterminated"#)?;

        let Err(error) = load_proxy_credentials(|_| None, &credentials_path) else {
            panic!("malformed credentials file should fail");
        };
        let ProxyConfigurationError::InvalidCredentialsFile { path, reason } = &error else {
            panic!("malformed credentials should use InvalidCredentialsFile");
        };
        assert_eq!(path, &credentials_path);
        assert_eq!(
            reason,
            "expected a non-empty JSON object whose keys and values are strings"
        );
        assert_eq!(
            error.to_string(),
            format!(
                "Invalid credentials file {}: expected a non-empty JSON object whose keys and values are strings",
                credentials_path.display()
            )
        );
        Ok(())
    }

    #[tokio::test]
    async fn chat_completion_validation_matches_swift_proxy() -> TestResult {
        for (body, expected) in [
            (
                json!({"model":"fast","messages":[]}),
                "Messages array must not be empty",
            ),
            (
                json!({"model":"fast","messages":[{"role":"tool","content":"bad role"}]}),
                "Invalid role: tool",
            ),
            (
                json!({"model":"fast","messages":[{"role":"system","content":"Be concise"}]}),
                "At least one user message is required",
            ),
        ] {
            let response = test_app()
                .oneshot(
                    Request::post("/v1/chat/completions")
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))?,
                )
                .await?;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert!(body_text(response).await?.contains(expected));
        }
        Ok(())
    }

    #[tokio::test]
    async fn non_streaming_chat_completion_uses_last_user_message() -> TestResult {
        let response = test_app()
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "fast",
                            "messages": [
                                {"role": "system", "content": "Be concise"},
                                {"role": "user", "content": "Ignore this"},
                                {"role": "assistant", "content": "Previous answer"},
                                {"role": "user", "content": "Use the final user message"}
                            ]
                        })
                        .to_string(),
                    ))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await?;
        assert_eq!(json["object"], "chat.completion");
        assert_eq!(json["model"], "fast");
        assert_eq!(json["choices"][0]["message"]["role"], "assistant");
        assert_eq!(json["choices"][0]["message"]["content"], "Proxy answer");
        assert_eq!(json["choices"][0]["finish_reason"], "stop");
        assert_eq!(json["usage"]["prompt_tokens"], 0);
        assert_eq!(
            json["usage"]["completion_tokens_details"]["reasoning_tokens"],
            0
        );
        assert_eq!(json["service_tier"], "default");
        Ok(())
    }

    #[test]
    fn streaming_chunks_match_swift_terminal_behavior() -> TestResult {
        let mut is_first_chunk = true;
        let mut emitted_content = false;
        let thinking = ConversationResponse {
            message: "hidden thought".to_string(),
            conversation_id: "conv-1".to_string(),
            response_id: "resp-1".to_string(),
            timestamp: None,
            web_search_results: None,
            xposts: None,
            is_thinking: true,
            is_soft_stop: false,
            is_final: false,
        };
        assert!(
            streaming_chunks(
                &thinking,
                "fast",
                "chunk-id",
                &mut is_first_chunk,
                &mut emitted_content
            )
            .is_empty()
        );

        let final_response = ConversationResponse::final_message(
            "Only final content.".to_string(),
            "conv-1".to_string(),
            "resp-1".to_string(),
            None,
            None,
            false,
        );
        let chunks = streaming_chunks(
            &final_response,
            "fast",
            "chunk-id",
            &mut is_first_chunk,
            &mut emitted_content,
        );
        assert_eq!(chunks.len(), 2);
        assert_eq!(
            chunks[0].choices[0].delta.role.as_deref(),
            Some("assistant")
        );
        assert_eq!(
            chunks[0].choices[0].delta.content.as_deref(),
            Some("Only final content.")
        );
        assert_eq!(chunks[1].choices[0].finish_reason.as_deref(), Some("stop"));
        assert_eq!(done_server_sent_event(), "data: [DONE]\n\n");
        assert_eq!(
            terminal_server_sent_events(true, "fast", "chunk-id")?,
            vec!["data: [DONE]\n\n".to_string()]
        );
        Ok(())
    }

    #[tokio::test]
    async fn streaming_chat_completion_emits_sse_chunks_and_done() -> TestResult {
        let response = test_app()
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "fast",
                            "stream": true,
                            "messages": [
                                {"role": "system", "content": "Be concise"},
                                {"role": "user", "content": "Use the final user message"}
                            ]
                        })
                        .to_string(),
                    ))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        let body = body_text(response).await?;
        assert!(body.contains("data: "));
        assert!(body.contains("Streaming answer"));
        assert!(!body.contains("Thinking"));
        assert!(body.contains("\"finish_reason\":\"stop\""));
        assert!(body.ends_with(done_server_sent_event()));
        Ok(())
    }

    #[tokio::test]
    async fn streaming_chat_backend_error_sse_escapes_json_quotes_and_newlines() -> TestResult {
        let mut backend = (*mock_backend()).clone();
        backend.stream_responses.clear();
        backend.stream_error = Some("backend said \"stop\"\nretry".to_string());
        let routed_backend: Arc<dyn GrokProxyBackend> = Arc::new(backend);

        let response = router(routed_backend)
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "fast",
                            "stream": true,
                            "messages": [
                                {"role": "user", "content": "Use the final user message"}
                            ]
                        })
                        .to_string(),
                    ))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await?;
        assert!(body.contains("\\\"stop\\\""));
        assert!(body.contains("\\nretry"));
        assert!(!body.contains("stop\"\nretry"));

        let event_json = body
            .strip_prefix("data: ")
            .and_then(|event| event.strip_suffix("\n\n"))
            .ok_or("stream error should be a single SSE data event")?;
        let event: Value = serde_json::from_str(event_json)?;
        assert_eq!(event["error"], "backend said \"stop\"\nretry");
        Ok(())
    }

    #[tokio::test]
    async fn audio_transcription_json_formats_match_swift_proxy() -> TestResult {
        let audio_base64 = BASE64_STANDARD.encode("audio bytes");
        let (app, backend) = test_app_with_backend();
        let response = app
            .clone()
            .oneshot(
                Request::post("/v1/audio/transcriptions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "grok-2-voice",
                            "audioBase64": audio_base64,
                            "audio_format": "wav",
                            "language": "en",
                            "prompt": "short clip"
                        })
                        .to_string(),
                    ))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await?;
        assert_eq!(json["text"], "hello audio");

        let response = app
            .oneshot(
                Request::post("/v1/audio/transcriptions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "grok-2-voice",
                            "audio_base64": audio_base64,
                            "response_format": "text",
                            "audio_format": "wav",
                            "language": "en",
                            "prompt": "short clip"
                        })
                        .to_string(),
                    ))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|content_type| content_type.starts_with("text/plain"))
        );
        assert_eq!(body_text(response).await?, "hello audio");

        let recorded = recorded_transcriptions(&backend)?;
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].model, "grok-2-voice");
        assert_eq!(recorded[0].audio_base64, audio_base64);
        assert_eq!(recorded[0].audio_format.as_deref(), Some("wav"));
        assert_eq!(recorded[0].language.as_deref(), Some("en"));
        assert_eq!(recorded[0].prompt.as_deref(), Some("short clip"));
        assert_eq!(recorded[1].response_format, "text");
        Ok(())
    }

    #[tokio::test]
    async fn audio_transcription_validation_matches_swift_proxy() -> TestResult {
        let (app, backend) = test_app_with_backend();
        for (body, expected) in [
            (
                json!({
                    "model": "grok-2-voice",
                    "audioBase64": null
                }),
                "JSON transcription requests require file, audioBase64, content, or data",
            ),
            (
                json!({
                    "model": "grok-2-voice",
                    "audioBase64": "not raw base64",
                    "audio_format": "webm"
                }),
                "JSON transcription audio must be raw base64",
            ),
            (
                json!({
                    "model": "grok-2-voice",
                    "audioBase64": "UklGRg==",
                    "response_format": "srt",
                    "audio_format": "wav"
                }),
                "Unsupported response_format: srt",
            ),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::post("/v1/audio/transcriptions")
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))?,
                )
                .await?;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert!(body_text(response).await?.contains(expected));
        }

        assert!(recorded_transcriptions(&backend)?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn audio_transcription_multipart_converts_file_to_base64() -> TestResult {
        let boundary = "Boundary-test";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ngrok-2-voice\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"response_format\"\r\n\r\njson\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"clip.webm\"\r\nContent-Type: audio/webm\r\n\r\naudio bytes\r\n--{boundary}--\r\n"
        );
        let (app, backend) = test_app_with_backend();
        let response = app
            .oneshot(
                Request::post("/v1/audio/transcriptions")
                    .header(
                        "content-type",
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await?;
        assert_eq!(json["text"], "hello audio");

        let recorded = recorded_transcriptions(&backend)?;
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].model, "grok-2-voice");
        assert_eq!(
            recorded[0].audio_base64,
            BASE64_STANDARD.encode("audio bytes")
        );
        assert_eq!(recorded[0].audio_format.as_deref(), Some("webm"));
        Ok(())
    }

    #[tokio::test]
    async fn audio_transcription_multipart_missing_model_returns_explicit_validation() -> TestResult
    {
        let boundary = "Boundary-missing-model";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"clip.webm\"\r\nContent-Type: audio/webm\r\n\r\naudio bytes\r\n--{boundary}--\r\n"
        );
        let (app, backend) = test_app_with_backend();
        let response = app
            .oneshot(
                Request::post("/v1/audio/transcriptions")
                    .header(
                        "content-type",
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(
            body_text(response)
                .await?
                .contains("Multipart transcription requests require model")
        );
        assert!(recorded_transcriptions(&backend)?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn audio_transcription_multipart_uses_mime_subtype_without_extension() -> TestResult {
        let boundary = "Boundary-mime";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ngrok-2-voice\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"recording\"\r\nContent-Type: audio/wav\r\n\r\nwav bytes\r\n--{boundary}--\r\n"
        );
        let (app, backend) = test_app_with_backend();
        let response = app
            .oneshot(
                Request::post("/v1/audio/transcriptions")
                    .header(
                        "content-type",
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await?;
        assert_eq!(json["text"], "hello audio");

        let recorded = recorded_transcriptions(&backend)?;
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            recorded[0].audio_base64,
            BASE64_STANDARD.encode("wav bytes")
        );
        assert_eq!(recorded[0].audio_format.as_deref(), Some("wav"));
        Ok(())
    }
}
