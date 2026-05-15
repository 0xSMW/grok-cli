use assert_cmd::Command;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

fn grok() -> Result<Command, Box<dyn std::error::Error>> {
    Ok(Command::cargo_bin("grok")?)
}

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
    fn spawn(body: impl Into<String>) -> Result<Self, Box<dyn std::error::Error>> {
        Self::spawn_sequence(vec![body])
    }

    fn spawn_recording_subscription(
        body: impl Into<String>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::spawn_sequence_with_subscription(Vec::<String>::new(), body, true)
    }

    fn spawn_sequence<I, S>(bodies: I) -> Result<Self, Box<dyn std::error::Error>>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::spawn_sequence_with_subscription(bodies, r#"{"subscriptions":[]}"#, false)
    }

    fn spawn_sequence_with_subscription<I, S>(
        bodies: I,
        subscription_body: impl Into<String>,
        record_subscription: bool,
    ) -> Result<Self, Box<dyn std::error::Error>>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let bodies = bodies.into_iter().map(Into::into).collect::<Vec<_>>();
        let subscription_body = subscription_body.into();
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || -> std::io::Result<()> {
            let mut body_index = 0;
            loop {
                if body_index >= bodies.len() && !bodies.is_empty() {
                    break;
                }
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
                if let (Some(headers_end), Some(content_length)) = (headers_end, content_length) {
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
                let recorded = RecordedRequest {
                    method,
                    path,
                    body: recorded_body,
                };
                let response_body = if recorded.method == "GET"
                    && recorded.path == "/rest/subscriptions"
                {
                    if record_subscription {
                        sender.send(recorded).map_err(|_| {
                            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "receiver closed")
                        })?;
                    }
                    subscription_body.as_str()
                } else {
                    sender.send(recorded).map_err(|_| {
                        std::io::Error::new(std::io::ErrorKind::BrokenPipe, "receiver closed")
                    })?;
                    let body = bodies.get(body_index).ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "mock server received more requests than responses",
                        )
                    })?;
                    body_index += 1;
                    body.as_str()
                };

                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                stream.write_all(response.as_bytes())?;
                if bodies.is_empty() || body_index >= bodies.len() {
                    break;
                }
            }
            Ok(())
        });

        Ok(Self {
            base_url: format!("http://{address}/rest"),
            receiver,
            handle,
        })
    }

    fn recorded_request(self) -> Result<RecordedRequest, Box<dyn std::error::Error>> {
        let mut requests = self.recorded_requests(1)?;
        requests
            .pop()
            .ok_or_else(|| std::io::Error::other("mock server did not record a request").into())
    }

    fn recorded_requests(
        self,
        count: usize,
    ) -> Result<Vec<RecordedRequest>, Box<dyn std::error::Error>> {
        let mut requests = Vec::new();
        for _ in 0..count {
            requests.push(self.receiver.recv_timeout(Duration::from_secs(5))?);
        }
        let join_result = self
            .handle
            .join()
            .map_err(|_| std::io::Error::other("mock server thread panicked"))?;
        join_result?;
        Ok(requests)
    }
}

struct StatusMockServer {
    base_url: String,
    receiver: Receiver<RecordedRequest>,
    handle: thread::JoinHandle<std::io::Result<()>>,
}

impl StatusMockServer {
    fn spawn_sequence<I, S>(responses: I) -> Result<Self, Box<dyn std::error::Error>>
    where
        I: IntoIterator<Item = (u16, S)>,
        S: Into<String>,
    {
        let responses = responses
            .into_iter()
            .map(|(status, body)| (status, body.into()))
            .collect::<Vec<_>>();
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || -> std::io::Result<()> {
            let mut response_index = 0;
            loop {
                if response_index >= responses.len() && !responses.is_empty() {
                    break;
                }
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
                if let (Some(headers_end), Some(content_length)) = (headers_end, content_length) {
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
                let recorded = RecordedRequest {
                    method,
                    path,
                    body: recorded_body,
                };
                let (status, response_body) =
                    if recorded.method == "GET" && recorded.path == "/rest/subscriptions" {
                        (200, r#"{"subscriptions":[]}"#.to_string())
                    } else {
                        let (status, body) = responses.get(response_index).ok_or_else(|| {
                            std::io::Error::new(
                                std::io::ErrorKind::UnexpectedEof,
                                "mock server received more requests than responses",
                            )
                        })?;
                        response_index += 1;
                        (*status, body.clone())
                    };
                sender.send(recorded).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "receiver closed")
                })?;

                let response = format!(
                    "HTTP/1.1 {} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    status,
                    reason_phrase(status),
                    response_body.len(),
                    response_body
                );
                stream.write_all(response.as_bytes())?;
                if responses.is_empty() || response_index >= responses.len() {
                    break;
                }
            }
            Ok(())
        });

        Ok(Self {
            base_url: format!("http://{address}/rest"),
            receiver,
            handle,
        })
    }

    fn recorded_requests(
        self,
        count: usize,
    ) -> Result<Vec<RecordedRequest>, Box<dyn std::error::Error>> {
        let mut requests = Vec::new();
        for _ in 0..count {
            requests.push(self.receiver.recv_timeout(Duration::from_secs(5))?);
        }
        let join_result = self
            .handle
            .join()
            .map_err(|_| std::io::Error::other("mock server thread panicked"))?;
        join_result?;
        Ok(requests)
    }
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Status",
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

fn json_line_events(output: Vec<u8>) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let stdout = String::from_utf8(output)?;
    stdout
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

#[test]
fn models_json_emits_single_result_envelope_without_human_banners()
-> Result<(), Box<dyn std::error::Error>> {
    let output = grok()?
        .args(["models", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;

    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "models");
    assert_eq!(json["category"], "model_list");
    assert_eq!(json["meta"]["format"], "json");
    assert_eq!(json["data"]["currentModel"]["id"], "fast");
    assert!(
        json["data"]["models"]
            .as_array()
            .is_some_and(|models| !models.is_empty())
    );
    assert!(!stdout.contains("Calling Grok API"));
    Ok(())
}

#[test]
fn models_json_loads_live_modes_like_swift() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"{"modes":[{"id":"fast","displayName":"Live Fast","summary":"Live default"},{"id":"live-mode","displayName":"Live Mode","summary":"From server"}]}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["models", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;
    let request = server.recorded_request()?;

    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/rest/modes");
    assert_eq!(serde_json::from_str::<Value>(&request.body)?, json!({}));
    assert_eq!(json["data"]["currentModel"]["displayName"], "Live Fast");
    assert_eq!(json["data"]["models"][1]["id"], "live-mode");
    assert_eq!(json["data"]["models"][1]["summary"], "From server");
    assert_eq!(json["data"]["models"][0]["selected"], true);
    Ok(())
}

#[test]
fn models_human_output_matches_swift_available_modes_listing()
-> Result<(), Box<dyn std::error::Error>> {
    let output = grok()?
        .arg("models")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;

    assert!(stdout.contains("Current model: Fast (fast)"));
    assert!(stdout.contains("Available web modes:"));
    assert!(stdout.contains("1. Auto (auto) - Chooses Fast or Expert"));
    assert!(stdout.contains("✓ 2. Fast (fast) - Quick responses"));
    assert!(
        stdout.contains("Grok 4.3 (beta) (grok-420-computer-use-sa) - Uses Skills and Connectors")
    );
    assert!(stdout.contains("You can also pass a raw web modeId with --model."));
    Ok(())
}

#[test]
fn modes_json_preserves_modes_command_name_like_swift() -> Result<(), Box<dyn std::error::Error>> {
    let output = grok()?
        .args(["modes", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output)?;

    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["command"], "modes");
    assert_eq!(json["category"], "model_list");
    assert!(
        json["data"]["models"]
            .as_array()
            .is_some_and(|models| !models.is_empty())
    );
    Ok(())
}

#[test]
fn test_json_matches_swift_harness_contract() -> Result<(), Box<dyn std::error::Error>> {
    let output = grok()?
        .args(["test", "--json", "hello", "parser"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;

    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "test");
    assert_eq!(json["category"], "test_result");
    assert_eq!(json["data"]["provided"], true);
    assert_eq!(json["data"]["message"], "hello parser");
    assert!(!stdout.contains("Test command executed successfully"));
    Ok(())
}

#[test]
fn test_human_output_and_help_match_swift_router_contract() -> Result<(), Box<dyn std::error::Error>>
{
    let run_output = grok()?
        .args(["test", "hello"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let run_stdout = String::from_utf8(run_output)?;

    assert!(run_stdout.contains("Test command executed successfully!"));
    assert!(run_stdout.contains(r#"Message provided: "hello""#));

    let help_output = grok()?
        .args(["test", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let help = String::from_utf8(help_output)?;

    assert!(help.contains("Usage: grok test [message...]"));
    assert!(!help.contains("Test command executed successfully"));
    assert!(!help.contains("Message provided"));
    Ok(())
}

#[test]
fn leading_json_option_before_command_matches_swift_router_behavior()
-> Result<(), Box<dyn std::error::Error>> {
    let output = grok()?
        .args(["--format", "json", "models"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output)?;

    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["command"], "models");
    Ok(())
}

#[test]
fn top_level_help_version_and_disabled_code_match_swift_router()
-> Result<(), Box<dyn std::error::Error>> {
    let help_output = grok()?
        .arg("help")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let help = String::from_utf8(help_output)?;
    assert!(help.contains("Usage: grok [command] [options]"));
    assert!(help.contains("message <text>"));
    assert!(help.contains("/workspace"));

    let help_json_output = grok()?
        .args(["--json", "help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let help_json: Value = serde_json::from_slice(&help_json_output)?;
    assert_eq!(help_json["schema"], "grok.cli.result.v1");
    assert_eq!(help_json["command"], "help");
    assert_eq!(help_json["category"], "help");
    assert!(
        help_json["data"]["commands"]
            .as_array()
            .is_some_and(|commands| commands.iter().any(|value| value == "message"))
    );
    assert!(
        help_json["data"]["interactiveCommands"]
            .as_array()
            .is_some_and(|commands| commands.iter().any(|value| value == "/workspace"))
    );

    let version_output = grok()?
        .arg("--version")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(String::from_utf8(version_output)?.contains("grok-cli"));

    let code_output = grok()?
        .env("GROK_CONFIG_DIR", tempfile::tempdir()?.path())
        .args(["--json", "code", "review", "this"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let code = String::from_utf8(code_output)?;
    assert!(code.contains("Grok Code is disabled"));
    assert!(!code.contains("schema"));
    Ok(())
}

#[test]
fn interactive_startup_uses_current_subscription_display_name_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn_recording_subscription(
        r#"{"subscriptions":[{"subscriptionTier":"TIER_SUPERGROK_HEAVY","status":"active"}]}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["chat"])
        .write_stdin("/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let request = server.recorded_request()?;

    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/rest/subscriptions");
    assert!(stdout.contains("Connected to SuperGrok Heavy!"));
    assert!(!stdout.contains("Connected to Grok!"));
    Ok(())
}

#[test]
fn interactive_forced_tty_flushes_prompt_and_streams_live_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"data: {"result":{"conversation":{"conversationId":"conv-live"},"response":{"responseId":"resp-live","token":"Hel"}}}
data: {"result":{"response":{"responseId":"resp-live","token":"lo"}}}
data: {"result":{"response":{"modelResponse":{"responseId":"resp-live","message":"Hello"}}}}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .env("GROK_CLI_FORCE_INTERACTIVE_TTY", "1")
        .args(["chat"])
        .write_stdin("hello live\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let clean_stdout = grok_cli::terminal::strip_ansi(&stdout);
    let request = server.recorded_request()?;
    let body: Value = serde_json::from_str(&request.body)?;

    assert!(clean_stdout.contains("Connected to Grok! Use / for commands, or type help."));
    assert!(clean_stdout.contains("Grok > Fast | MD"));
    assert!(clean_stdout.contains("> "));
    assert!(clean_stdout.contains("\nGrok\nHello"));
    assert!(clean_stdout.contains("Goodbye!"));
    assert_eq!(request.path, "/rest/app-chat/conversations/new");
    assert_eq!(body["message"], "hello live");
    Ok(())
}

#[test]
fn command_help_uses_swift_usage_instead_of_generated_clap_help()
-> Result<(), Box<dyn std::error::Error>> {
    let message_help = String::from_utf8(
        grok()?
            .args(["message", "--help"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )?;
    let chat_help = String::from_utf8(
        grok()?
            .args(["chat", "-h"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )?;
    let transcribe_help = String::from_utf8(
        grok()?
            .args(["transcribe", "help"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )?;
    let models_help = String::from_utf8(
        grok()?
            .args(["models", "--help"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )?;

    assert!(message_help.contains("Usage: grok message [options] [message...]"));
    assert!(message_help.contains("--prompt-file <path>"));
    assert!(!message_help.contains("Usage: grok message [ARGS]"));
    assert!(chat_help.contains("Usage: grok chat [options] [initial message...]"));
    assert!(chat_help.contains("--audio <path|->"));
    assert!(!chat_help.contains("Usage: grok chat [ARGS]"));
    assert!(transcribe_help.contains("Usage: grok transcribe [options] <path|->"));
    assert!(transcribe_help.contains("Examples:"));
    assert!(!transcribe_help.contains("Usage: grok transcribe [ARGS]"));
    assert!(models_help.contains("Usage: grok models [--json|--format json]"));
    assert!(models_help.contains("Fast (fast)"));
    assert!(!models_help.contains("Usage: grok models [OPTIONS]"));
    Ok(())
}

#[test]
fn usage_errors_exit_with_swift_status_two() -> Result<(), Box<dyn std::error::Error>> {
    let cases = [
        (
            vec!["message", "--format"],
            vec!["Error:", "requires a format value"],
        ),
        (
            vec!["message", "--format=", "hello"],
            vec!["Error:", "requires a format value"],
        ),
        (
            vec!["message", "--format=bogus", "hello"],
            vec!["Error:", "Invalid output format"],
        ),
        (
            vec!["message", "--model"],
            vec!["Error:", "requires a model value"],
        ),
        (
            vec!["message", "--model=", "hello"],
            vec!["Error:", "requires a model value"],
        ),
        (
            vec!["chat", "--mode="],
            vec!["Error:", "requires a model value"],
        ),
        (
            vec!["chat", "--format", "xml"],
            vec!["Error:", "Invalid output format"],
        ),
        (
            vec!["chat", "--stdin"],
            vec!["Error:", "--stdin is only supported by grok message"],
        ),
        (vec!["transcribe"], vec!["Error:", "Usage: grok transcribe"]),
    ];

    for (args, fragments) in cases {
        let output = grok()?
            .args(args.clone())
            .assert()
            .failure()
            .code(2)
            .get_output()
            .stdout
            .clone();
        let stdout = String::from_utf8(output)?;
        for fragment in fragments {
            assert!(
                stdout.contains(fragment),
                "{args:?} output {stdout:?} did not contain {fragment:?}"
            );
        }
        assert!(!stdout.contains("Calling Grok API"));
    }
    Ok(())
}

#[test]
fn resource_commands_reject_non_json_format_like_swift() -> Result<(), Box<dyn std::error::Error>> {
    let invalid_cases = [
        vec!["list", "--format", "raw"],
        vec!["files", "list", "--format=raw"],
        vec!["skills", "mine", "--format", "raw"],
        vec!["workspaces", "list", "--format=raw"],
        vec!["tasks", "list", "--format", "raw"],
        vec!["agents", "list", "--format=raw"],
        vec!["--format", "raw", "list"],
    ];

    for args in invalid_cases {
        let output = grok()?
            .args(args.clone())
            .assert()
            .failure()
            .code(2)
            .get_output()
            .stdout
            .clone();
        let stdout = String::from_utf8(output)?;

        assert!(
            stdout.contains("Invalid --format value: raw. Use json."),
            "{args:?} output {stdout:?}"
        );
        assert!(!stdout.contains("Calling Grok API"));
    }

    let missing_cases = [vec!["list", "--format"], vec!["tasks", "list", "--format"]];
    for args in missing_cases {
        let output = grok()?
            .args(args.clone())
            .assert()
            .failure()
            .code(2)
            .get_output()
            .stdout
            .clone();
        let stdout = String::from_utf8(output)?;

        assert!(
            stdout.contains("--format requires a value"),
            "{args:?} output {stdout:?}"
        );
        assert!(!stdout.contains("Calling Grok API"));
    }

    let output = grok()?
        .args(["agents", "list", "--format="])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    assert!(stdout.contains("--format requires a value"));
    assert!(!stdout.contains("Calling Grok API"));

    Ok(())
}

#[test]
fn transcribe_rejects_markdown_output_like_swift() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let audio_file = temp_dir.path().join("clip.webm");
    std::fs::write(&audio_file, "audio bytes")?;
    let audio_path = audio_file.to_str().unwrap_or_default();

    for args in [
        vec!["transcribe", "--markdown", audio_path],
        vec!["transcribe", "-m", audio_path],
        vec!["transcribe", "--format", "md", audio_path],
    ] {
        let output = grok()?
            .env("GROK_CONFIG_DIR", temp_dir.path())
            .args(args.clone())
            .assert()
            .failure()
            .code(2)
            .get_output()
            .stdout
            .clone();
        let stdout = String::from_utf8(output)?;

        assert!(
            stdout.contains("Use raw or json"),
            "{args:?} output {stdout:?}"
        );
        assert!(!stdout.contains("Transcribing audio"));
    }
    Ok(())
}

#[test]
fn transcribe_treats_non_swift_md_flag_as_unknown_audio_option()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let audio_file = temp_dir.path().join("clip.webm");
    std::fs::write(&audio_file, "audio bytes")?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .args([
            "transcribe",
            "--md",
            audio_file.to_str().unwrap_or_default(),
        ])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;

    assert!(stdout.contains("Unknown audio option: --md"));
    assert!(!stdout.contains("Transcribing audio"));
    Ok(())
}

#[test]
fn json_usage_errors_exit_with_embedded_status_like_swift() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let unknown_audio = temp_dir.path().join("clip.audio");
    std::fs::write(&unknown_audio, "audio bytes")?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .args([
            "transcribe",
            "--json",
            unknown_audio.to_str().unwrap_or_default(),
        ])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output)?;

    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], false);
    assert_eq!(json["command"], "transcribe");
    assert_eq!(json["category"], "error");
    assert_eq!(json["error"]["code"], "usage_error");
    assert_eq!(json["error"]["exitCode"], 2);
    assert!(
        json["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("audio format"))
    );
    Ok(())
}

#[test]
fn bare_message_json_posts_new_conversation_and_emits_assistant_response()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"data: {"result":{"conversation":{"conversationId":"conv-1"},"response":{"modelResponse":{"responseId":"resp-1","message":"Hello from Grok"}}}}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["--json", "hello", "from", "rust"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output)?;
    let request = server.recorded_request()?;
    let body: Value = serde_json::from_str(&request.body)?;

    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/rest/app-chat/conversations/new");
    assert_eq!(body["message"], "hello from rust");
    assert_eq!(body["modeId"], "fast");
    assert_eq!(body["temporary"], false);
    assert_eq!(body["fileAttachments"], json!([]));
    assert_eq!(body["enableImageGeneration"], true);
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["command"], "message");
    assert_eq!(json["category"], "assistant_response");
    assert_eq!(json["data"]["message"], "Hello from Grok");
    assert_eq!(json["data"]["conversationId"], "conv-1");
    assert_eq!(json["data"]["responseId"], "resp-1");
    assert_eq!(json["data"]["request"]["reasoning"], true);
    assert_eq!(json["data"]["request"]["stream"], false);
    Ok(())
}

#[test]
fn message_text_output_defaults_to_markdown_and_raw_preserves_source_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn_sequence(vec![
        r##"data: {"result":{"conversation":{"conversationId":"conv-md"},"response":{"modelResponse":{"responseId":"resp-md","message":"# CLI Heading\n**bold** and [source](https://example.com)"}}}}"##,
        r##"data: {"result":{"conversation":{"conversationId":"conv-raw"},"response":{"modelResponse":{"responseId":"resp-raw","message":"# CLI Heading\n**bold** and [source](https://example.com)"}}}}"##,
    ])?;

    let markdown_output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["message", "hello"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let raw_output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["message", "--raw", "hello"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let markdown = String::from_utf8(markdown_output)?;
    let raw = String::from_utf8(raw_output)?;
    let _requests = server.recorded_requests(2)?;

    assert!(markdown.contains("CLI Heading"));
    assert!(markdown.contains("bold and source"));
    assert!(!markdown.contains("# CLI Heading"));
    assert!(!markdown.contains("**bold**"));
    assert!(!markdown.contains("[source](https://example.com)"));
    assert!(raw.contains("# CLI Heading"));
    assert!(raw.contains("**bold**"));
    assert!(raw.contains("[source](https://example.com)"));
    Ok(())
}

#[test]
fn message_raw_quiet_strips_grok_render_markup_like_swift() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let answer = r#"# Source Heading
Keep **bold** and [source](https://example.com) syntax.
Hide <grok:render type="render_inline_citation"><argument name="citation_id">5</argument></grok:render> citations.
Hide escaped <grok:render type=\"render_inline_citation\"><argument name=\"citation_id\">5</argument></grok:render> fragments.
Hide residual _id="ccee26" card_type="citation_card" type="render_inline_citation"><argument name="citation_id">5</argument></grok:render> fragments.
Hide escaped residual _id=\"ccee26\" card_type=\"citation_card\" type=\"render_inline_citation\"><argument name=\"citation_id\">5</argument></grok:render> fragments."#;
    let stream_body = format!(
        r#"data: {{"result":{{"conversation":{{"conversationId":"conv-raw"}},"response":{{"modelResponse":{{"responseId":"resp-raw","message":{}}}}}}}}}"#,
        serde_json::to_string(answer)?
    );
    let server = JsonMockServer::spawn(stream_body)?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["message", "--raw", "--quiet", "hello"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let _request = server.recorded_request()?;

    assert!(stdout.contains("# Source Heading"));
    assert!(stdout.contains("**bold**"));
    assert!(stdout.contains("[source](https://example.com)"));
    assert!(!stdout.contains("<grok:render"));
    assert!(!stdout.contains("render_inline_citation"));
    assert!(!stdout.contains("citation_card"));
    assert!(!stdout.contains("ccee26"));
    assert!(!stdout.contains("<argument"));
    assert!(!stdout.contains(r#"\"citation_id\""#));
    Ok(())
}

#[test]
fn message_raw_quiet_reads_stdin_and_prompt_file_exactly_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let prompt_file = temp_dir.path().join("prompt.txt");
    let stdin_prompt = "first stdin line\nsecond stdin line\n";
    let file_prompt = "first file line\nsecond file line\n";
    std::fs::write(&prompt_file, file_prompt)?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"data: {"result":{"conversation":{"conversationId":"conv-stdin"},"response":{"modelResponse":{"responseId":"resp-stdin","message":"stdin answer"}}}}"#,
        r#"data: {"result":{"conversation":{"conversationId":"conv-file"},"response":{"modelResponse":{"responseId":"resp-file","message":"file answer"}}}}"#,
    ])?;

    let stdin_output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["message", "--raw", "--quiet"])
        .write_stdin(stdin_prompt)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let file_output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args([
            "message",
            "--raw",
            "--quiet",
            "--prompt-file",
            prompt_file.to_str().unwrap_or_default(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let requests = server.recorded_requests(2)?;
    let stdin_body: Value = serde_json::from_str(&requests[0].body)?;
    let file_body: Value = serde_json::from_str(&requests[1].body)?;

    assert_eq!(String::from_utf8(stdin_output)?.trim_end(), "stdin answer");
    assert_eq!(String::from_utf8(file_output)?.trim_end(), "file answer");
    assert_eq!(stdin_body["message"], stdin_prompt);
    assert_eq!(file_body["message"], file_prompt);
    Ok(())
}

#[test]
fn message_markdown_final_output_strips_grok_render_markup_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let answer = r#"# Cited Heading
Final answer <grok:render type="render_inline_citation"><argument name="citation_id">5</argument></grok:render> still reads cleanly.
Escaped fragment <grok:render type=\"render_inline_citation\"><argument name=\"citation_id\">5</argument></grok:render> is hidden too."#;
    let stream_body = format!(
        r#"data: {{"result":{{"conversation":{{"conversationId":"conv-md"}},"response":{{"modelResponse":{{"responseId":"resp-md","message":{}}}}}}}}}"#,
        serde_json::to_string(answer)?
    );
    let server = JsonMockServer::spawn(stream_body)?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["message", "hello"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let _request = server.recorded_request()?;

    assert!(stdout.contains("Cited Heading"));
    assert!(stdout.contains("Final answer  still reads cleanly."));
    assert!(stdout.contains("Escaped fragment  is hidden too."));
    assert!(!stdout.contains("# Cited Heading"));
    assert!(!stdout.contains("<grok:render"));
    assert!(!stdout.contains("render_inline_citation"));
    assert!(!stdout.contains("<argument"));
    assert!(!stdout.contains(r#"\"citation_id\""#));
    Ok(())
}

#[test]
fn message_legacy_reasoning_and_search_flags_warn_without_changing_payload_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"data: {"result":{"conversation":{"conversationId":"conv-legacy"},"response":{"modelResponse":{"responseId":"resp-legacy","message":"legacy answer"}}}}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args([
            "message",
            "--reasoning",
            "--deep-search",
            "--no-search",
            "--no-custom-instructions",
            "--private",
            "--model",
            "expert",
            "hello",
            "there",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let request = server.recorded_request()?;
    let body: Value = serde_json::from_str(&request.body)?;

    assert!(stdout.contains("--reasoning is deprecated and ignored"));
    assert!(stdout.contains("--deep-search is deprecated and ignored"));
    assert!(stdout.contains("--no-search is deprecated and ignored"));
    assert!(stdout.contains("--no-custom-instructions is deprecated and ignored"));
    assert!(stdout.contains("legacy answer"));
    assert_eq!(body["message"], "hello there");
    assert_eq!(body["modeId"], "expert");
    assert_eq!(body["temporary"], true);
    assert!(body.get("disableSearch").is_none());
    assert!(body.get("customPersonality").is_none());
    Ok(())
}

#[test]
fn message_audio_json_transcribes_then_sends_transcript_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let audio_file = temp_dir.path().join("voice.webm");
    std::fs::write(&audio_file, "voice bytes")?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"{"text":"message audio transcript"}"#,
        r#"data: {"result":{"conversation":{"conversationId":"conv-audio-message"},"response":{"modelResponse":{"responseId":"resp-audio-message","message":"audio message answer"}}}}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args([
            "message",
            "--audio",
            audio_file.to_str().unwrap_or_default(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output)?;
    let requests = server.recorded_requests(2)?;
    let transcription_body: Value = serde_json::from_str(&requests[0].body)?;
    let chat_body: Value = serde_json::from_str(&requests[1].body)?;

    assert_eq!(requests[0].path, "/rest/voice/speech-to-text");
    assert_eq!(transcription_body["audioBase64"], "dm9pY2UgYnl0ZXM=");
    assert_eq!(transcription_body["audioFormat"], "webm");
    assert_eq!(requests[1].path, "/rest/app-chat/conversations/new");
    assert_eq!(chat_body["message"], "message audio transcript");
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["command"], "message");
    assert_eq!(json["data"]["message"], "audio message answer");
    assert_eq!(json["data"]["input"]["kind"], "audio");
    assert_eq!(
        json["data"]["input"]["transcript"],
        "message audio transcript"
    );
    assert_eq!(json["data"]["input"]["audio"]["format"], "webm");
    Ok(())
}

#[test]
fn message_audio_stream_json_emits_transcription_before_request_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let audio_file = temp_dir.path().join("stream-note.webm");
    std::fs::write(&audio_file, "audio bytes")?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"{"text":"stream audio transcript"}"#,
        r#"data: {"result":{"conversation":{"conversationId":"conv-audio-stream"},"response":{"modelResponse":{"responseId":"resp-audio-stream","message":"stream answer"}}}}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args([
            "message",
            "--audio",
            audio_file.to_str().unwrap_or_default(),
            "--stream",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let events = json_line_events(output)?;
    let requests = server.recorded_requests(2)?;
    let transcription_body: Value = serde_json::from_str(&requests[0].body)?;
    let chat_body: Value = serde_json::from_str(&requests[1].body)?;

    assert!(events.len() >= 4);
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event["schema"], "grok.cli.event.v1");
        assert_eq!(event["sequence"], index + 1);
    }
    assert_eq!(requests[0].path, "/rest/voice/speech-to-text");
    assert_eq!(transcription_body["audioBase64"], "YXVkaW8gYnl0ZXM=");
    assert_eq!(transcription_body["audioFormat"], "webm");
    assert_eq!(requests[1].path, "/rest/app-chat/conversations/new");
    assert_eq!(chat_body["message"], "stream audio transcript");

    assert_eq!(events[0]["event"], "transcription");
    assert_eq!(events[0]["data"]["kind"], "audio");
    assert_eq!(events[0]["data"]["transcript"], "stream audio transcript");
    assert_eq!(
        events[0]["data"]["audio"]["path"],
        audio_file.to_str().unwrap_or_default()
    );
    assert_eq!(events[0]["data"]["audio"]["format"], "webm");

    assert_eq!(events[1]["event"], "request");
    assert_eq!(events[1]["data"]["message"], "stream audio transcript");
    assert_eq!(events[1]["data"]["input"]["kind"], "audio");
    assert_eq!(events[1]["data"]["request"]["stream"], true);

    let final_event = events
        .iter()
        .find(|event| event["event"] == "assistant_final")
        .ok_or_else(|| std::io::Error::other("missing assistant_final event"))?;
    assert_eq!(final_event["data"]["message"], "stream answer");
    assert_eq!(final_event["data"]["input"]["kind"], "audio");
    assert_eq!(
        final_event["data"]["input"]["transcript"],
        "stream audio transcript"
    );
    Ok(())
}

#[test]
fn message_file_upload_json_uploads_then_attaches_returned_id()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let upload_file = temp_dir.path().join("notes.txt");
    std::fs::write(&upload_file, "hello")?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"{"fileMetadataId":"file-1","fileName":"notes.txt","asset":{"assetId":"asset-1","fileName":"notes.txt","mimeType":"text/plain"}}"#,
        r#"data: {"result":{"conversation":{"conversationId":"conv-1"},"response":{"modelResponse":{"responseId":"resp-1","message":"Attached file response"}}}}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args([
            "--json",
            "--attach",
            "existing-1",
            "--file",
            upload_file.to_str().unwrap_or_default(),
            "message",
            "summarize",
            "attachment",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output)?;
    let requests = server.recorded_requests(2)?;
    let upload_body: Value = serde_json::from_str(&requests[0].body)?;
    let message_body: Value = serde_json::from_str(&requests[1].body)?;

    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/rest/app-chat/upload-file");
    assert_eq!(upload_body["fileName"], "notes.txt");
    assert_eq!(upload_body["fileMimeType"], "text/plain");
    assert_eq!(upload_body["content"], "aGVsbG8=");
    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[1].path, "/rest/app-chat/conversations/new");
    assert_eq!(message_body["message"], "summarize attachment");
    assert_eq!(
        message_body["fileAttachments"],
        json!(["existing-1", "file-1"])
    );
    assert_eq!(
        json["data"]["request"]["fileAttachmentIds"],
        json!(["existing-1", "file-1"])
    );
    assert_eq!(json["data"]["message"], "Attached file response");
    Ok(())
}

#[test]
fn message_stream_json_emits_swift_style_ndjson_events() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"data: {"result":{"conversation":{"conversationId":"conv-1"},"response":{"modelResponse":{"responseId":"resp-1","message":"Streaming final response"}}}}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["message", "--stream", "--json", "hello"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let events = stdout
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let request = server.recorded_request()?;
    let body: Value = serde_json::from_str(&request.body)?;

    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/rest/app-chat/conversations/new");
    assert_eq!(body["message"], "hello");
    assert_eq!(events.len(), 4);
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event["schema"], "grok.cli.event.v1");
        assert_eq!(event["sequence"], index + 1);
    }
    assert_eq!(events[0]["event"], "request");
    assert_eq!(events[0]["data"]["message"], "hello");
    assert_eq!(events[0]["data"]["request"]["stream"], true);
    assert_eq!(events[1]["event"], "progress");
    assert_eq!(events[2]["event"], "assistant_final");
    assert_eq!(events[2]["data"]["message"], "Streaming final response");
    assert_eq!(events[2]["data"]["conversationId"], "conv-1");
    assert_eq!(events[2]["data"]["responseId"], "resp-1");
    assert_eq!(events[3]["event"], "done");
    assert_eq!(events[3]["data"]["ok"], true);
    Ok(())
}

#[test]
fn message_stream_json_suppresses_generic_thinking_placeholder_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"data: {"result":{"conversation":{"conversationId":"conv-thinking"},"response":{"responseId":"resp-thinking","token":"Thinking about your request","isThinking":true}}}
data: {"result":{"response":{"responseId":"resp-thinking","token":"answer"}}}
data: {"result":{"response":{"modelResponse":{"responseId":"resp-thinking","message":"answer"}}}}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["message", "--stream", "--json", "hello"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let events = json_line_events(output)?;
    let event_names = events
        .iter()
        .filter_map(|event| event["event"].as_str())
        .collect::<Vec<_>>();
    let _request = server.recorded_request()?;

    assert!(!event_names.contains(&"thinking_start"));
    assert!(!event_names.contains(&"thinking_delta"));
    assert!(!event_names.contains(&"thinking_end"));
    assert_eq!(
        events
            .iter()
            .find(|event| event["event"] == "assistant_delta")
            .and_then(|event| event["data"]["text"].as_str()),
        Some("answer")
    );
    assert_eq!(event_names.last().copied(), Some("done"));
    Ok(())
}

#[test]
fn message_stream_json_emits_thinking_lifecycle_and_trace_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"data: {"result":{"conversation":{"conversationId":"conv-thinking"},"response":{"responseId":"resp-thinking","token":"Example calculation","isThinking":true}}}
data: {"result":{"response":{"responseId":"resp-thinking","token":"answer"}}}
data: {"result":{"response":{"modelResponse":{"responseId":"resp-thinking","message":"answer"}}}}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["message", "--stream", "--json", "hello"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let events = json_line_events(output)?;
    let event_names = events
        .iter()
        .filter_map(|event| event["event"].as_str())
        .collect::<Vec<_>>();
    let _request = server.recorded_request()?;

    assert!(event_names.contains(&"thinking_start"));
    assert!(event_names.contains(&"thinking_delta"));
    assert!(event_names.contains(&"thinking_end"));
    assert_eq!(
        events
            .iter()
            .find(|event| event["event"] == "thinking_delta")
            .and_then(|event| event["data"]["text"].as_str()),
        Some("Example calculation")
    );
    assert!(events.iter().any(|event| {
        event["event"] == "trace"
            && event["data"]["kind"] == "thinking"
            && event["data"]["text"] == "Example calculation"
    }));
    assert_eq!(event_names.last().copied(), Some("done"));
    Ok(())
}

#[test]
fn message_stream_json_suppresses_split_residual_citation_fragments_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"data: {"result":{"conversation":{"conversationId":"conv-cite"},"response":{"responseId":"resp-cite","token":"Lead _id=\"ccee26\" card_type=\"citation_card\" type=\"render_inline_citation\"><arg"}}}
data: {"result":{"response":{"responseId":"resp-cite","token":"ument name=\"citation_id\">5</argument></grok:render> tail"}}}
data: {"result":{"response":{"modelResponse":{"responseId":"resp-cite","message":"Lead  tail"}}}}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["message", "--stream", "--json", "hello"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let events = stdout
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let deltas = events
        .iter()
        .filter(|event| event["event"] == "assistant_delta")
        .filter_map(|event| event["data"]["text"].as_str())
        .collect::<String>();
    let _request = server.recorded_request()?;

    assert_eq!(deltas, "Lead  tail");
    assert!(!stdout.contains("ccee26"));
    assert!(!stdout.contains("citation_card"));
    assert_eq!(
        events
            .iter()
            .find(|event| event["event"] == "assistant_final")
            .and_then(|event| event["data"]["message"].as_str()),
        Some("Lead  tail")
    );
    Ok(())
}

#[test]
fn message_stream_json_emits_tool_activity_and_trace_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"data: {"result":{"conversation":{"conversationId":"conv-activity"},"response":{"responseId":"resp-activity","token":"<xai:tool_usage_card><xai:tool_name>web_search</xai:tool_name><xai:tool_args><![CDATA[{\"query\":\"swift terminal UI\"}]]></xai:tool_args></xai:tool_usage_card>","isThinking":true}}}
data: {"result":{"response":{"modelResponse":{"responseId":"resp-activity","message":"answer"}}}}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["message", "--stream", "--json", "hello"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let events = json_line_events(output)?;
    let _request = server.recorded_request()?;

    assert!(events.iter().any(|event| {
        event["event"] == "activity"
            && event["data"]["kind"] == "search"
            && event["data"]["text"] == "swift terminal UI"
    }));
    assert!(events.iter().any(|event| {
        event["event"] == "trace"
            && event["data"]["kind"] == "search"
            && event["data"]["text"] == "swift terminal UI"
    }));
    assert!(
        !events
            .iter()
            .any(|event| event["event"] == "thinking_delta")
    );
    Ok(())
}

#[test]
fn chat_raw_quiet_piped_input_creates_then_continues_conversation()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"data: {"result":{"conversation":{"conversationId":"conv-1"},"response":{"modelResponse":{"responseId":"resp-1","message":"first answer"}}}}"#,
        r#"data: {"result":{"response":{"modelResponse":{"responseId":"resp-2","message":"second answer"}}}}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["chat", "--raw", "--quiet"])
        .write_stdin("first\nsecond\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let requests = server.recorded_requests(2)?;
    let create_body: Value = serde_json::from_str(&requests[0].body)?;
    let continue_body: Value = serde_json::from_str(&requests[1].body)?;

    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/rest/app-chat/conversations/new");
    assert_eq!(create_body["message"], "first");
    assert_eq!(requests[1].method, "POST");
    assert_eq!(
        requests[1].path,
        "/rest/app-chat/conversations/conv-1/responses"
    );
    assert_eq!(continue_body["message"], "second");
    assert_eq!(stdout.trim_end(), "first answer\nsecond answer");
    assert!(!stdout.contains("Connected to"));
    assert!(!stdout.contains("Sending message"));
    Ok(())
}

#[test]
fn interactive_auth_error_refreshes_credentials_and_keeps_session_alive_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"expired","x-anonuserid":"old"}"#,
    )?;
    let extractor = temp_dir.path().join("fake_refresh_cookie_extractor.py");
    std::fs::write(
        &extractor,
        r#"
import json
import sys
args = sys.argv[1:]
output = args[args.index("--output") + 1]
with open(output, "w") as handle:
    json.dump({"x-anonuserid": "refreshed", "sso": "cookie"}, handle)
"#,
    )?;
    let server = StatusMockServer::spawn_sequence(vec![
        (401, r#"{"error":"unauthorized"}"#),
        (
            200,
            r#"data: {"result":{"conversation":{"conversationId":"conv-refreshed"},"response":{"modelResponse":{"responseId":"resp-refreshed","message":"refreshed answer"}}}}"#,
        ),
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_COOKIE_EXTRACTOR", &extractor)
        .env("GROK_BASE_URL", &server.base_url)
        .args(["chat"])
        .write_stdin("first message with expired cookies\nsecond message after refresh\nquit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let requests = server.recorded_requests(3)?;
    let conversation_requests = requests
        .iter()
        .filter(|request| request.path.contains("/rest/app-chat/conversations"))
        .collect::<Vec<_>>();

    assert!(
        stdout.contains("Authentication failed. Your saved Grok browser cookies may have expired.")
    );
    assert!(stdout.contains("Trying to refresh credentials from your browser..."));
    assert!(stdout.contains("Successfully refreshed credentials from browser."));
    assert!(stdout.contains("Retry your last message or command."));
    assert!(stdout.contains("refreshed answer"));
    assert!(
        std::fs::read_to_string(temp_dir.path().join("credentials.json"))?.contains("refreshed")
    );
    assert_eq!(conversation_requests.len(), 2);
    assert_eq!(
        conversation_requests[0].path,
        "/rest/app-chat/conversations/new"
    );
    assert_eq!(
        conversation_requests[1].path,
        "/rest/app-chat/conversations/new"
    );
    Ok(())
}

#[test]
fn interactive_share_creates_link_and_copies_like_swift() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let clipboard_path = temp_dir.path().join("clipboard.txt");
    let share_url = "https://grok.com/share/share-created-rust";
    let server = JsonMockServer::spawn_sequence(vec![
        r#"data: {"result":{"conversation":{"conversationId":"conv-1"},"response":{"modelResponse":{"responseId":"resp-1","message":"hello answer"}}}}"#,
        r#"{"shareLinks":[]}"#,
        r#"{"shareLinkId":"share-created-rust"}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .env("GROK_CLIPBOARD_FILE", &clipboard_path)
        .args(["chat"])
        .write_stdin("hello\n/share\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let requests = server.recorded_requests(3)?;
    let create_share_body: Value = serde_json::from_str(&requests[2].body)?;

    assert!(stdout.contains(&format!("Copied share link {share_url}")));
    assert_eq!(std::fs::read_to_string(clipboard_path)?, share_url);
    assert_eq!(requests[0].path, "/rest/app-chat/conversations/new");
    assert_eq!(
        requests[1].path,
        "/rest/app-chat/share_links?pageSize=100&conversationId=conv-1&responseId=resp-1"
    );
    assert_eq!(
        requests[2].path,
        "/rest/app-chat/conversations/conv-1/share"
    );
    assert_eq!(create_share_body["responseId"], "resp-1");
    assert_eq!(create_share_body["allowIndexing"], true);
    Ok(())
}

#[test]
fn interactive_delete_soft_deletes_current_conversation_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"data: {"result":{"conversation":{"conversationId":"conv-1"},"response":{"modelResponse":{"responseId":"resp-1","message":"hello answer"}}}}"#,
        r#"{}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["chat"])
        .write_stdin("hello\n/delete --yes\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let requests = server.recorded_requests(2)?;

    assert!(stdout.contains("Deleted conversation conv-1."));
    assert_eq!(requests[0].path, "/rest/app-chat/conversations/new");
    assert_eq!(requests[1].method, "DELETE");
    assert_eq!(requests[1].path, "/rest/app-chat/conversations/soft/conv-1");
    Ok(())
}

#[test]
fn interactive_delete_requires_yes_when_not_tty_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .args(["chat", "--quiet"])
        .write_stdin("/delete\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;

    assert!(stdout.contains("Usage: /delete --yes"));
    assert!(!stdout.contains("Deleted conversation"));
    Ok(())
}

#[test]
fn interactive_delete_rejects_extra_arguments_like_swift() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .args(["chat", "--quiet"])
        .write_stdin("/delete now\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;

    assert!(stdout.contains("Usage: /delete [--yes]"));
    assert!(!stdout.contains("Deleted conversation"));
    Ok(())
}

#[test]
fn interactive_model_command_changes_subsequent_mode_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"data: {"result":{"conversation":{"conversationId":"conv-1"},"response":{"modelResponse":{"responseId":"resp-1","message":"mode answer"}}}}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["chat"])
        .write_stdin("/mode raw-interactive-mode\nhello\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let request = server.recorded_request()?;
    let body: Value = serde_json::from_str(&request.body)?;

    assert!(stdout.contains("Model set to: raw-interactive-mode (raw-interactive-mode)"));
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/rest/app-chat/conversations/new");
    assert_eq!(body["message"], "hello");
    assert_eq!(body["modeId"], "raw-interactive-mode");
    Ok(())
}

#[test]
fn interactive_private_on_starts_new_private_thread_after_existing_conversation_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"data: {"result":{"conversation":{"conversationId":"conv-1"},"response":{"modelResponse":{"responseId":"resp-1","message":"saved answer"}}}}"#,
        r#"data: {"result":{"conversation":{"conversationId":"conv-private"},"response":{"modelResponse":{"responseId":"resp-private","message":"private answer"}}}}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["chat"])
        .write_stdin("saved thread message\n/private on\nprivate thread message\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let requests = server.recorded_requests(2)?;
    let saved_body: Value = serde_json::from_str(&requests[0].body)?;
    let private_body: Value = serde_json::from_str(&requests[1].body)?;

    assert!(stdout.contains("Started a new private conversation thread."));
    assert!(stdout.contains("Private mode: ENABLED"));
    assert_eq!(requests[0].path, "/rest/app-chat/conversations/new");
    assert_eq!(requests[1].path, "/rest/app-chat/conversations/new");
    assert_eq!(saved_body["message"], "saved thread message");
    assert_eq!(saved_body["temporary"], false);
    assert_eq!(private_body["message"], "private thread message");
    assert_eq!(private_body["temporary"], true);
    Ok(())
}

#[test]
fn interactive_skill_create_starts_grok43_skill_creator_thread_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"data: {"result":{"conversation":{"conversationId":"conv-1"},"response":{"modelResponse":{"responseId":"resp-1","message":"normal answer"}}}}"#,
        r#"data: {"result":{"conversation":{"conversationId":"skill-conv"},"response":{"modelResponse":{"responseId":"skill-resp","message":"skill created"}}}}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["chat"])
        .write_stdin("normal message\n/skill create summarize invoices\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let requests = server.recorded_requests(2)?;
    let normal_body: Value = serde_json::from_str(&requests[0].body)?;
    let skill_body: Value = serde_json::from_str(&requests[1].body)?;

    assert!(stdout.contains("skill created"));
    assert_eq!(requests[0].path, "/rest/app-chat/conversations/new");
    assert_eq!(requests[1].path, "/rest/app-chat/conversations/new");
    assert_eq!(normal_body["message"], "normal message");
    assert_eq!(
        skill_body["message"],
        "skill-creator skill summarize invoices"
    );
    assert_eq!(skill_body["modeId"], "grok-420-computer-use-sa");
    Ok(())
}

#[test]
fn interactive_format_and_stream_toggles_match_swift_session_state()
-> Result<(), Box<dyn std::error::Error>> {
    let output = grok()?
        .args(["chat", "--quiet"])
        .write_stdin(
            "/stream off\n/stream on\n/reasoning\n/reason off\n/typeahead\n/typeahead off\n/typeahead enable\n/format raw\n/md on\n/raw on\n/quit\n",
        )
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;

    assert!(stdout.contains("Streaming: DISABLED"));
    assert!(stdout.contains("Streaming: ENABLED"));
    assert_eq!(
        stdout
            .matches("/reason is deprecated and ignored since the Grok 4 release")
            .count(),
        2
    );
    assert!(stdout.contains("Typeahead: DISABLED"));
    assert_eq!(stdout.matches("Typeahead: ENABLED").count(), 2);
    assert!(stdout.contains("Output format: Raw"));
    assert!(stdout.contains("Output format: Markdown"));
    assert_eq!(stdout.matches("Output format: Raw").count(), 2);
    Ok(())
}

#[test]
fn interactive_bare_commands_and_quoted_args_match_swift_parser_boundaries()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"data: {"result":{"conversation":{"conversationId":"conv-bare"},"response":{"modelResponse":{"responseId":"resp-bare","message":"chat boundary answer"}}}}"#,
        r#"{"task":{"taskId":"task-bare","name":"Bare Task","prompt":"bare quoted prompt","isEnabled":true}}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["chat", "--quiet"])
        .write_stdin(
            "reasoning on\nprivate off\nstream on\nmodels list\nmode expert\nmodel this should remain chat\ntasks create --prompt \"bare quoted prompt\" --name \"Bare Task\" --json\n/quit\n",
        )
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let requests = server.recorded_requests(2)?;
    let chat_body: Value = serde_json::from_str(&requests[0].body)?;
    let task_body: Value = serde_json::from_str(&requests[1].body)?;

    assert!(stdout.contains("/reason is deprecated and ignored"));
    assert!(stdout.contains("Private mode: DISABLED"));
    assert!(stdout.contains("Streaming: ENABLED"));
    assert!(stdout.contains("Model set to: Expert (expert)"));
    assert!(stdout.contains("chat boundary answer"));
    assert_eq!(requests[0].path, "/rest/app-chat/conversations/new");
    assert_eq!(chat_body["message"], "model this should remain chat");
    assert_eq!(chat_body["modeId"], "expert");
    assert_eq!(requests[1].path, "/rest/tasks");
    assert_eq!(task_body["prompt"], "bare quoted prompt");
    assert_eq!(task_body["name"], "Bare Task");
    Ok(())
}

#[test]
fn interactive_unknown_slash_command_suggests_nearest_match_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let output = grok()?
        .args(["chat", "--quiet"])
        .write_stdin("/wrkspace\n/wat\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;

    assert!(stdout.contains("Unknown command /wrkspace"));
    assert!(stdout.contains("Did you mean /workspace"));
    assert!(stdout.contains("Unknown command /wat"));
    assert!(!stdout.contains("Did you mean /wat"));
    Ok(())
}

#[test]
fn interactive_help_uses_grouped_registry_output_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let output = grok()?
        .args(["chat", "--quiet"])
        .write_stdin("/help\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;

    assert!(stdout.contains("Basic Commands:"));
    assert!(stdout.contains("Slash Commands:"));
    assert!(stdout.contains("Session:"));
    assert!(stdout.contains("Model:"));
    assert!(stdout.contains("Files:"));
    assert!(stdout.contains("Workspace:"));
    assert!(stdout.contains("Library:"));
    assert!(stdout.contains("Auth:"));
    assert!(stdout.contains("Audio:"));
    assert!(stdout.contains("Utility:"));
    assert!(stdout.contains("- /agents [list|show|edit|set]: Manage agent settings"));
    assert!(!stdout.contains("Agent Commands:"));
    assert!(!stdout.contains("sync-custom"));
    Ok(())
}

#[test]
fn interactive_limits_fetches_rate_limit_summary_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"{"modelName":"fast","remainingResponses":42,"resetAfterSeconds":900}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["chat"])
        .write_stdin("/limits\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let request = server.recorded_request()?;
    let body: Value = serde_json::from_str(&request.body)?;

    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/rest/rate-limits");
    assert_eq!(body["modelName"], "fast");
    assert!(stdout.contains("Rate limits for Fast (fast)"));
    assert!(stdout.contains("Remaining responses: 42 responses"));
    assert!(stdout.contains("Resets in 15m"));
    Ok(())
}

#[test]
fn interactive_limits_falls_back_to_unavailable_summary_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn("not json")?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["chat"])
        .write_stdin("/limits\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let request = server.recorded_request()?;

    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/rest/rate-limits");
    assert!(stdout.contains("Rate limits for Fast (fast)"));
    assert!(stdout.contains("Current rate-limit data is unavailable."));
    assert!(!stdout.contains("Error:"));
    Ok(())
}

#[test]
fn interactive_goal_completion_marker_stops_loop_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"data: {"result":{"conversation":{"conversationId":"goal-conv"},"response":{"modelResponse":{"responseId":"goal-resp","message":"Audit complete\n<grok_goal status=\"complete\">"}}}}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["chat"])
        .write_stdin("/goal write release note\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let request = server.recorded_request()?;
    let body: Value = serde_json::from_str(&request.body)?;
    let message = body["message"].as_str().unwrap_or_default();

    assert!(stdout.contains("Goal started"));
    assert!(stdout.contains("Goal complete"));
    assert_eq!(request.path, "/rest/app-chat/conversations/new");
    assert!(message.contains("<grok_goal_request>"));
    assert!(message.contains("write release note"));
    assert!(message.contains(r#"<grok_goal status="complete">"#));
    Ok(())
}

#[test]
fn interactive_goal_continues_until_max_turns_like_swift() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"data: {"result":{"conversation":{"conversationId":"goal-conv"},"response":{"modelResponse":{"responseId":"goal-resp-1","message":"Still working"}}}}"#,
        r#"data: {"result":{"response":{"modelResponse":{"responseId":"goal-resp-2","message":"Still working"}}}}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["chat"])
        .write_stdin("/goal --max-turns 2 ship docs\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let requests = server.recorded_requests(2)?;
    let first_body: Value = serde_json::from_str(&requests[0].body)?;
    let second_body: Value = serde_json::from_str(&requests[1].body)?;
    let first_message = first_body["message"].as_str().unwrap_or_default();
    let second_message = second_body["message"].as_str().unwrap_or_default();

    assert!(stdout.contains("Goal stopped after 2 turns."));
    assert_eq!(requests[0].path, "/rest/app-chat/conversations/new");
    assert_eq!(
        requests[1].path,
        "/rest/app-chat/conversations/goal-conv/responses"
    );
    assert!(first_message.contains("<grok_goal_request>"));
    assert!(second_message.contains("<grok_goal_continuation>"));
    assert!(second_message.contains("Goal loop turn 2 of 2"));
    Ok(())
}

#[test]
fn interactive_goal_new_clears_goal_like_swift() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"data: {"result":{"conversation":{"conversationId":"goal-conv"},"response":{"modelResponse":{"responseId":"goal-resp","message":"Still working"}}}}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["chat"])
        .write_stdin("/goal --max-turns 1 lingering task\n/new\n/goal\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let request = server.recorded_request()?;

    assert!(stdout.contains("Goal stopped after 1 turns."));
    assert!(stdout.contains("Started a new conversation thread."));
    assert!(stdout.contains("No active goal."));
    assert_eq!(request.path, "/rest/app-chat/conversations/new");
    Ok(())
}

#[test]
fn interactive_resume_seeds_leaf_parent_and_mode_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"{"conversations":[{"conversationId":"conv-branch","title":"Branch Chat","modifyTime":"2026-05-15T01:00:00Z"}]}"#,
        r#"{"responseNodes":[{"responseId":"root-user","sender":"human"},{"responseId":"old-assistant","sender":"assistant","parentResponseId":"root-user"},{"responseId":"leaf-assistant","sender":"assistant","parentResponseId":"root-user"}]}"#,
        r#"{"responses":[{"responseId":"root-user","sender":"human","message":"Original question","createTime":"2026-05-15T01:00:00Z"},{"responseId":"old-assistant","sender":"assistant","message":"Older branch","createTime":"2026-05-15T01:01:00Z","parentResponseId":"root-user","modeId":"fast"},{"responseId":"leaf-assistant","sender":"assistant","message":"Latest branch","createTime":"2026-05-15T01:02:00Z","parentResponseId":"root-user","modeId":"expert"}]}"#,
        r#"data: {"result":{"response":{"modelResponse":{"responseId":"resp-follow-up","message":"continued answer"}}}}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["chat"])
        .write_stdin("/resume\n1\nfollow up\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let requests = server.recorded_requests(4)?;
    let load_body: Value = serde_json::from_str(&requests[2].body)?;
    let continue_body: Value = serde_json::from_str(&requests[3].body)?;

    assert!(stdout.contains("Available conversations:"));
    assert!(stdout.contains("Loading conversation \"Branch Chat\"..."));
    assert!(stdout.contains("Model: Expert (expert)"));
    assert!(stdout.contains("Grok: Latest branch"));
    assert_eq!(
        requests[0].path,
        "/rest/app-chat/conversations?pageSize=100"
    );
    assert_eq!(
        requests[1].path,
        "/rest/app-chat/conversations/conv-branch/response-node"
    );
    assert_eq!(
        requests[2].path,
        "/rest/app-chat/conversations/conv-branch/load-responses"
    );
    assert_eq!(
        load_body["responseIds"],
        json!(["root-user", "old-assistant", "leaf-assistant"])
    );
    assert_eq!(
        requests[3].path,
        "/rest/app-chat/conversations/conv-branch/responses"
    );
    assert_eq!(continue_body["message"], "follow up");
    assert_eq!(continue_body["parentResponseId"], "leaf-assistant");
    assert_eq!(continue_body["modeId"], "expert");
    Ok(())
}

#[test]
fn interactive_search_routes_query_before_selection_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"{"conversations":[{"conversationId":"conv-search","title":"Search Hit"}]}"#,
        r#"{"responseNodes":[{"responseId":"resp-search","sender":"assistant"}]}"#,
        r#"{"responses":[{"responseId":"resp-search","sender":"assistant","message":"Search history","createTime":"2026-05-15T01:00:00Z"}]}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["chat"])
        .write_stdin("/search project chat\n1\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let requests = server.recorded_requests(3)?;

    assert!(stdout.contains("Search Hit"));
    assert!(
        requests[0]
            .path
            .starts_with("/rest/app-chat/conversations?pageSize=60&searchQuery=project")
    );
    assert!(requests[0].path.contains("chat"));
    assert_eq!(
        requests[1].path,
        "/rest/app-chat/conversations/conv-search/response-node"
    );
    assert_eq!(
        requests[2].path,
        "/rest/app-chat/conversations/conv-search/load-responses"
    );
    Ok(())
}

#[test]
fn interactive_resource_slash_commands_delegate_to_ported_handlers_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"{"assets":[{"fileMetadataId":"file-chat","fileName":"chat.txt","mimeType":"text/plain"}]}"#,
        r#"{"tasks":[{"taskId":"task-1","title":"Paused Task","status":"paused"}]}"#,
        r#"{"userSkills":[{"skillId":"user-skill-1","name":"My Skill","description":"Custom skill"}]}"#,
        r#"{"agentCustomizations":{"values":[{"agentId":1,"name":"Research","instructions":"Use citations"}]}}"#,
        r#"{"workspaces":[{"workspaceId":"workspace-1","name":"Research"}]}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["chat"])
        .write_stdin(
            "/files\n/tasks inactive\n/skills mine\n/agents\n/workspaces list\n/auth help\n/clear\n/quit\n",
        )
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let requests = server.recorded_requests(5)?;

    assert!(stdout.contains("Files:"));
    assert!(stdout.contains("Paused Task"));
    assert!(stdout.contains("User Skills"));
    assert!(stdout.contains("Agents:"));
    assert!(stdout.contains("Workspaces:"));
    assert!(stdout.contains("Auth commands:"));
    assert!(stdout.contains("import <file> - Import credentials from a JSON file"));
    assert_eq!(
        requests[0].path,
        "/rest/assets?pageSize=9&orderBy=ORDER_BY_LAST_USE_TIME"
    );
    assert_eq!(requests[1].path, "/rest/tasks/inactive");
    assert_eq!(requests[2].path, "/rest/user-skills");
    assert_eq!(requests[3].path, "/rest/user-settings");
    assert_eq!(
        requests[4].path,
        "/rest/workspaces?pageSize=50&orderBy=ORDER_BY_LAST_USE_TIME"
    );
    Ok(())
}

#[test]
fn interactive_attach_id_applies_to_next_message_then_clears_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"data: {"result":{"conversation":{"conversationId":"conv-attach"},"response":{"modelResponse":{"responseId":"resp-attach","message":"attached answer"}}}}"#,
        r#"data: {"result":{"response":{"modelResponse":{"responseId":"resp-next","message":"plain answer"}}}}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["chat"])
        .write_stdin("/attach existing-file\nwith attach\nwithout attach\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let requests = server.recorded_requests(2)?;
    let attached_body: Value = serde_json::from_str(&requests[0].body)?;
    let plain_body: Value = serde_json::from_str(&requests[1].body)?;

    assert!(stdout.contains("Attached file ID: existing-file"));
    assert_eq!(requests[0].path, "/rest/app-chat/conversations/new");
    assert_eq!(attached_body["message"], "with attach");
    assert_eq!(attached_body["fileAttachments"], json!(["existing-file"]));
    assert_eq!(
        requests[1].path,
        "/rest/app-chat/conversations/conv-attach/responses"
    );
    assert_eq!(plain_body["message"], "without attach");
    assert_eq!(plain_body["fileAttachments"], json!([]));
    Ok(())
}

#[test]
fn interactive_attach_picker_uses_id_display_fallback_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(r#"{"assets":[{"fileMetadataId":"file-only-id"}]}"#)?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["chat"])
        .write_stdin("/attach\n1\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let request = server.recorded_request()?;

    assert_eq!(
        request.path,
        "/rest/assets?pageSize=25&orderBy=ORDER_BY_LAST_USE_TIME"
    );
    assert!(stdout.contains("Select file to attach:"));
    assert!(stdout.contains("1. file-only-id file-only-id"));
    assert!(stdout.contains("Attached: file-only-id"));
    Ok(())
}

#[test]
fn interactive_workspace_selection_resets_and_scopes_next_message_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"data: {"result":{"conversation":{"conversationId":"conv-before"},"response":{"modelResponse":{"responseId":"resp-before","message":"before answer"}}}}"#,
        r#"{"workspaces":[{"workspaceId":"workspace-1","name":"Research"}]}"#,
        r#"data: {"result":{"conversation":{"conversationId":"conv-workspace"},"response":{"modelResponse":{"responseId":"resp-workspace","message":"workspace answer"}}}}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["chat"])
        .write_stdin("before\n/workspace\n1\nscoped message\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let requests = server.recorded_requests(3)?;
    let before_body: Value = serde_json::from_str(&requests[0].body)?;
    let scoped_body: Value = serde_json::from_str(&requests[2].body)?;

    assert!(stdout.contains("Select workspace:"));
    assert!(stdout.contains("Workspace set to: Research"));
    assert_eq!(requests[0].path, "/rest/app-chat/conversations/new");
    assert_eq!(before_body["message"], "before");
    assert!(before_body.get("workspaceIds").is_none());
    assert_eq!(
        requests[1].path,
        "/rest/workspaces?pageSize=50&orderBy=ORDER_BY_LAST_USE_TIME"
    );
    assert_eq!(requests[2].path, "/rest/app-chat/conversations/new");
    assert_eq!(scoped_body["message"], "scoped message");
    assert_eq!(scoped_body["workspaceIds"], json!(["workspace-1"]));
    assert_eq!(scoped_body.get("parentResponseId"), None);
    Ok(())
}

#[test]
fn interactive_workspace_picker_uses_id_display_fallback_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(r#"{"workspaces":[{"workspaceId":"workspace-only-id"}]}"#)?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["chat"])
        .write_stdin("/workspace\n1\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let request = server.recorded_request()?;

    assert_eq!(
        request.path,
        "/rest/workspaces?pageSize=50&orderBy=ORDER_BY_LAST_USE_TIME"
    );
    assert!(stdout.contains("Select workspace:"));
    assert!(stdout.contains("1. workspace-only-id workspace-only-id"));
    assert!(stdout.contains("Workspace set to: workspace-only-id"));
    Ok(())
}

#[test]
fn interactive_audio_send_transcribes_then_sends_transcript_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let audio_file = temp_dir.path().join("clip.webm");
    std::fs::write(&audio_file, "webm bytes")?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"{"text":"voice transcript"}"#,
        r#"data: {"result":{"conversation":{"conversationId":"conv-audio"},"response":{"modelResponse":{"responseId":"resp-audio","message":"voice answer"}}}}"#,
    ])?;

    let stdin = format!(
        "/audio send {}\n/quit\n",
        audio_file.to_str().unwrap_or_default()
    );

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["chat"])
        .write_stdin(stdin)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let requests = server.recorded_requests(2)?;
    let transcription_body: Value = serde_json::from_str(&requests[0].body)?;
    let chat_body: Value = serde_json::from_str(&requests[1].body)?;

    assert!(stdout.contains("Transcribing audio..."));
    assert!(stdout.contains("[transcript] voice transcript"));
    assert!(stdout.contains("voice answer"));
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/rest/voice/speech-to-text");
    assert_eq!(transcription_body["audioBase64"], "d2VibSBieXRlcw==");
    assert_eq!(transcription_body["audioFormat"], "webm");
    assert_eq!(
        transcription_body["refinementLevel"],
        "REFINEMENT_LEVEL_POLISH"
    );
    assert_eq!(requests[1].path, "/rest/app-chat/conversations/new");
    assert_eq!(chat_body["message"], "voice transcript");
    assert_eq!(chat_body["fileAttachments"], json!([]));
    Ok(())
}

#[test]
fn chat_audio_initial_message_transcribes_then_sends_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let audio_file = temp_dir.path().join("chat-note.webm");
    std::fs::write(&audio_file, "chat audio bytes")?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"{"text":"chat audio transcript"}"#,
        r#"data: {"result":{"conversation":{"conversationId":"conv-chat-audio"},"response":{"modelResponse":{"responseId":"resp-chat-audio","message":"chat audio answer"}}}}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args([
            "chat",
            "--audio",
            audio_file.to_str().unwrap_or_default(),
            "--raw",
            "--quiet",
        ])
        .write_stdin("/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let requests = server.recorded_requests(2)?;
    let transcription_body: Value = serde_json::from_str(&requests[0].body)?;
    let chat_body: Value = serde_json::from_str(&requests[1].body)?;

    assert_eq!(requests[0].path, "/rest/voice/speech-to-text");
    assert_eq!(
        transcription_body["audioBase64"],
        "Y2hhdCBhdWRpbyBieXRlcw=="
    );
    assert_eq!(transcription_body["audioFormat"], "webm");
    assert_eq!(requests[1].path, "/rest/app-chat/conversations/new");
    assert_eq!(chat_body["message"], "chat audio transcript");
    assert!(stdout.contains("chat audio answer"));
    Ok(())
}

#[test]
fn interactive_audio_records_from_fixture_when_no_path_is_provided_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let fixture_file = temp_dir.path().join("recording-fixture.webm");
    std::fs::write(&fixture_file, "recorded bytes")?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"{"text":"recorded audio transcript"}"#,
        r#"data: {"result":{"conversation":{"conversationId":"conv-record"},"response":{"modelResponse":{"responseId":"resp-record-1","message":"recorded answer one"}}}}"#,
        r#"{"text":"recorded audio transcript"}"#,
        r#"data: {"result":{"conversation":{"conversationId":"conv-record"},"response":{"modelResponse":{"responseId":"resp-record-2","message":"recorded answer two"}}}}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .env("GROK_CLI_AUDIO_RECORD_FIXTURE", &fixture_file)
        .args(["chat", "--raw", "--quiet"])
        .write_stdin("/audio\n/audio send\n/quit\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let requests = server.recorded_requests(4)?;
    let first_transcription_body: Value = serde_json::from_str(&requests[0].body)?;
    let first_chat_body: Value = serde_json::from_str(&requests[1].body)?;
    let second_transcription_body: Value = serde_json::from_str(&requests[2].body)?;
    let second_chat_body: Value = serde_json::from_str(&requests[3].body)?;

    assert_eq!(requests[0].path, "/rest/voice/speech-to-text");
    assert_eq!(
        first_transcription_body["audioBase64"],
        "cmVjb3JkZWQgYnl0ZXM="
    );
    assert_eq!(first_transcription_body["audioFormat"], "webm");
    assert_eq!(requests[2].path, "/rest/voice/speech-to-text");
    assert_eq!(
        second_transcription_body["audioBase64"],
        "cmVjb3JkZWQgYnl0ZXM="
    );
    assert_eq!(second_transcription_body["audioFormat"], "webm");
    assert_eq!(first_chat_body["message"], "recorded audio transcript");
    assert_eq!(second_chat_body["message"], "recorded audio transcript");
    assert!(stdout.contains("recorded answer one"));
    assert!(stdout.contains("recorded answer two"));
    Ok(())
}

#[test]
fn auth_import_json_writes_credentials_without_human_banners()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let import_file = temp_dir.path().join("imported-credentials.json");
    std::fs::write(&import_file, r#"{"sso":"cookie","x-anonuserid":"anon"}"#)?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .args([
            "auth",
            "import",
            import_file.to_str().unwrap_or_default(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;

    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "auth");
    assert_eq!(json["subcommand"], "import");
    assert_eq!(json["category"], "auth_result");
    assert_eq!(json["data"]["action"], "import");
    assert!(!stdout.contains("Importing credentials from"));
    assert!(!stdout.contains("Successfully imported credentials"));
    assert_eq!(
        std::fs::read_to_string(temp_dir.path().join("credentials.json"))?,
        r#"{"sso":"cookie","x-anonuserid":"anon"}"#
    );
    Ok(())
}

#[test]
fn auth_import_human_prints_swift_progress_banners() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let import_file = temp_dir.path().join("imported-credentials.json");
    std::fs::write(&import_file, r#"{"sso":"cookie","x-anonuserid":"anon"}"#)?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .args(["auth", "import", import_file.to_str().unwrap_or_default()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;

    assert!(stdout.contains(&format!(
        "Importing credentials from {}...",
        import_file.display()
    )));
    assert!(stdout.contains("Successfully imported credentials!"));
    assert_eq!(
        std::fs::read_to_string(temp_dir.path().join("credentials.json"))?,
        r#"{"sso":"cookie","x-anonuserid":"anon"}"#
    );
    Ok(())
}

#[test]
fn auth_help_lists_swift_browser_shortcuts() -> Result<(), Box<dyn std::error::Error>> {
    let output = grok()?
        .args(["auth", "help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;

    assert!(stdout.contains("Auth commands:"));
    assert!(stdout.contains("auth          - Generate new credentials from browser cookies"));
    assert!(stdout.contains("safari        - Generate credentials from Safari"));
    assert!(stdout.contains("chrome        - Generate credentials from Chrome"));
    assert!(stdout.contains("auto, safari, atlas, chrome, firefox, chromium, brave, edge, arc"));
    assert!(stdout.contains("Add --json, --format json, or --format=json for JSON output"));
    Ok(())
}

#[test]
fn auth_import_keeps_non_json_format_args_like_swift() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let import_file = temp_dir.path().join("imported-credentials.json");
    std::fs::write(&import_file, r#"{"sso":"cookie","x-anonuserid":"anon"}"#)?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .args([
            "auth",
            "import",
            "--format",
            "raw",
            import_file.to_str().unwrap_or_default(),
        ])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;

    assert!(stdout.contains("Error: Please provide a path to the credentials file"));
    assert!(!temp_dir.path().join("credentials.json").exists());
    Ok(())
}

#[test]
fn auth_import_invalid_json_returns_error_envelope() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let import_file = temp_dir.path().join("invalid-credentials.json");
    std::fs::write(&import_file, r#"{"not":"credentials"}"#)?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .args([
            "auth",
            "import",
            import_file.to_str().unwrap_or_default(),
            "--json",
        ])
        .assert()
        .failure()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output)?;

    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], false);
    assert_eq!(json["command"], "auth");
    assert_eq!(json["subcommand"], "import");
    assert_eq!(json["category"], "error");
    assert!(
        json["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("auth cookie"))
    );
    Ok(())
}

#[test]
fn auth_generate_json_runs_cookie_extractor_and_suppresses_noise()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let extractor = temp_dir.path().join("fake_cookie_extractor.py");
    std::fs::write(
        &extractor,
        r#"
import json
import sys
print("extractor stdout noise")
print("extractor stderr noise", file=sys.stderr)
args = sys.argv[1:]
output = args[args.index("--output") + 1]
with open(output, "w") as handle:
    json.dump({"x-anonuserid": "generated", "sso": "cookie"}, handle)
"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_COOKIE_EXTRACTOR", &extractor)
        .args(["auth", "chrome", "--json", "--quiet"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;

    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "auth");
    assert_eq!(json["subcommand"], "generate");
    assert_eq!(json["category"], "auth_result");
    assert_eq!(json["data"]["action"], "generate");
    assert_eq!(json["data"]["browser"], "chrome");
    assert!(
        json["data"]["credentialsPath"]
            .as_str()
            .is_some_and(|path| path.ends_with("credentials.json"))
    );
    assert!(!stdout.contains("extractor stdout noise"));
    assert!(!stdout.contains("extractor stderr noise"));
    assert_eq!(
        std::fs::read_to_string(temp_dir.path().join("credentials.json"))?,
        r#"{"x-anonuserid": "generated", "sso": "cookie"}"#
    );
    Ok(())
}

#[test]
fn auth_generate_human_prints_swift_progress_banners() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let extractor = temp_dir.path().join("fake_cookie_extractor.py");
    std::fs::write(
        &extractor,
        r#"
import json
import sys
args = sys.argv[1:]
output = args[args.index("--output") + 1]
with open(output, "w") as handle:
    json.dump({"x-anonuserid": "generated", "sso": "cookie"}, handle)
"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_COOKIE_EXTRACTOR", &extractor)
        .args(["auth", "chrome"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;

    assert!(stdout.contains("Extracting credentials from browser..."));
    assert!(stdout.contains("Successfully generated credentials!"));
    assert!(stdout.contains("Saved to:"));
    assert!(stdout.contains("credentials.json"));
    assert_eq!(
        std::fs::read_to_string(temp_dir.path().join("credentials.json"))?,
        r#"{"x-anonuserid": "generated", "sso": "cookie"}"#
    );
    Ok(())
}

#[test]
fn files_list_json_matches_swift_resource_list_contract() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"{"assets":[{"fileMetadataId":"file-1","fileName":"notes.txt","mimeType":"text/plain"},{"asset_id":"asset-2","name":"image.png","mime_type":"image/png"}]}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["files", "list", "--page-size", "2", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;
    let request = server.recorded_request()?;

    assert_eq!(request.method, "GET");
    assert_eq!(
        request.path,
        "/rest/assets?pageSize=2&orderBy=ORDER_BY_LAST_USE_TIME"
    );
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "files");
    assert_eq!(json["subcommand"], "list");
    assert_eq!(json["category"], "resource_list");
    assert_eq!(json["data"]["resource"], "file");
    assert_eq!(json["data"]["pageSize"], 2);
    assert_eq!(json["data"]["items"][0]["id"], "file-1");
    assert_eq!(json["data"]["items"][0]["fileName"], "notes.txt");
    assert_eq!(json["data"]["items"][0]["mimeType"], "text/plain");
    assert_eq!(json["data"]["items"][1]["id"], "asset-2");
    assert_eq!(json["data"]["items"][1]["fileName"], "image.png");
    assert!(!stdout.contains("Files:"));
    Ok(())
}

#[test]
fn files_subcommand_help_matches_swift_usage_contract() -> Result<(), Box<dyn std::error::Error>> {
    let cases = [
        (["files", "list", "--help"], "Usage: grok files list"),
        (["files", "upload", "--help"], "Usage: grok files upload"),
        (["files", "delete", "--help"], "Usage: grok files delete"),
        (["files", "remove", "--help"], "Usage: grok files delete"),
    ];

    for (args, expected) in cases {
        let output = grok()?
            .args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let stdout = String::from_utf8(output)?;
        assert!(
            stdout.contains(expected),
            "{stdout:?} did not contain {expected:?}"
        );
        assert!(!stdout.contains("Error:"), "{stdout:?}");
    }
    Ok(())
}

#[test]
fn files_upload_json_matches_swift_resource_mutation_contract()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let upload_file = temp_dir.path().join("notes.txt");
    std::fs::write(&upload_file, "hello")?;
    let server = JsonMockServer::spawn(
        r#"{"fileMetadataId":"file-1","fileName":"notes.txt","asset":{"assetId":"asset-1","fileName":"notes.txt","mimeType":"text/plain"}}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args([
            "files",
            "upload",
            upload_file.to_str().unwrap_or_default(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;
    let request = server.recorded_request()?;
    let body: Value = serde_json::from_str(&request.body)?;

    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/rest/app-chat/upload-file");
    assert_eq!(body["fileName"], "notes.txt");
    assert_eq!(body["fileMimeType"], "text/plain");
    assert_eq!(body["content"], "aGVsbG8=");
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "files");
    assert_eq!(json["subcommand"], "upload");
    assert_eq!(json["category"], "resource_mutation");
    assert_eq!(json["data"]["resource"], "file");
    assert_eq!(json["data"]["action"], "upload");
    assert_eq!(json["data"]["id"], "file-1");
    assert_eq!(json["data"]["item"]["id"], "file-1");
    assert_eq!(json["data"]["item"]["fileName"], "notes.txt");
    assert_eq!(json["data"]["item"]["asset"]["id"], "asset-1");
    assert!(!stdout.contains("Uploaded file"));
    Ok(())
}

#[test]
fn files_delete_json_matches_swift_resource_mutation_contract()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"{"asset":{"fileMetadataId":"file-1","fileName":"notes.txt","mimeType":"text/plain"}}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["files", "delete", "file-1", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;
    let request = server.recorded_request()?;

    assert_eq!(request.method, "DELETE");
    assert_eq!(request.path, "/rest/assets/file-1");
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "files");
    assert_eq!(json["subcommand"], "delete");
    assert_eq!(json["category"], "resource_mutation");
    assert_eq!(json["data"]["resource"], "file");
    assert_eq!(json["data"]["action"], "delete");
    assert_eq!(json["data"]["id"], "file-1");
    assert_eq!(json["data"]["completed"], true);
    assert_eq!(json["data"]["item"]["id"], "file-1");
    assert_eq!(json["data"]["item"]["fileName"], "notes.txt");
    assert!(!stdout.contains("Deleted file"));
    Ok(())
}

#[test]
fn transcribe_json_matches_swift_transcription_contract() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let audio_file = temp_dir.path().join("clip.m4a");
    std::fs::write(&audio_file, "m4a bytes")?;
    let server = JsonMockServer::spawn(r#"{"text":"json audio transcript"}"#)?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args([
            "transcribe",
            "--json",
            audio_file.to_str().unwrap_or_default(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;
    let request = server.recorded_request()?;
    let body: Value = serde_json::from_str(&request.body)?;

    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/rest/voice/speech-to-text");
    assert_eq!(body["audioBase64"], "bTRhIGJ5dGVz");
    assert_eq!(body["audioFormat"], "m4a");
    assert_eq!(body["refinementLevel"], "REFINEMENT_LEVEL_POLISH");
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "transcribe");
    assert_eq!(json["category"], "transcription");
    assert_eq!(json["data"]["kind"], "audio");
    assert_eq!(json["data"]["transcript"], "json audio transcript");
    assert_eq!(
        json["data"]["audio"]["path"],
        audio_file.to_str().unwrap_or_default()
    );
    assert_eq!(json["data"]["audio"]["format"], "m4a");
    assert_eq!(
        json["data"]["audio"]["refinementLevel"],
        "REFINEMENT_LEVEL_POLISH"
    );
    assert!(!stdout.contains("Transcribing audio"));
    Ok(())
}

#[test]
fn transcribe_human_prints_progress_and_quiet_suppresses_it_like_swift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let audio_file = temp_dir.path().join("clip.m4a");
    std::fs::write(&audio_file, "m4a bytes")?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"{"text":"human audio transcript"}"#,
        r#"{"text":"quiet audio transcript"}"#,
    ])?;

    let human_output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["transcribe", audio_file.to_str().unwrap_or_default()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let quiet_output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args([
            "transcribe",
            "--quiet",
            audio_file.to_str().unwrap_or_default(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human_stdout = String::from_utf8(human_output)?;
    let quiet_stdout = String::from_utf8(quiet_output)?;
    let requests = server.recorded_requests(2)?;

    assert_eq!(requests[0].path, "/rest/voice/speech-to-text");
    assert_eq!(requests[1].path, "/rest/voice/speech-to-text");
    assert!(human_stdout.contains("Transcribing audio..."));
    assert!(human_stdout.contains("human audio transcript"));
    assert!(!quiet_stdout.contains("Transcribing audio..."));
    assert_eq!(quiet_stdout.trim_end(), "quiet audio transcript");
    Ok(())
}

#[test]
fn transcribe_accepts_swift_raw_format_aliases() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let audio_file = temp_dir.path().join("clip.m4a");
    std::fs::write(&audio_file, "m4a bytes")?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"{"text":"plain alias transcript"}"#,
        r#"{"text":"text alias transcript"}"#,
    ])?;

    let plain_output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args([
            "transcribe",
            "--quiet",
            "--format=plain",
            audio_file.to_str().unwrap_or_default(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text_output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args([
            "transcribe",
            "--quiet",
            "--format",
            "text",
            audio_file.to_str().unwrap_or_default(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let plain_stdout = String::from_utf8(plain_output)?;
    let text_stdout = String::from_utf8(text_output)?;
    let requests = server.recorded_requests(2)?;

    assert_eq!(requests[0].path, "/rest/voice/speech-to-text");
    assert_eq!(requests[1].path, "/rest/voice/speech-to-text");
    assert_eq!(plain_stdout.trim_end(), "plain alias transcript");
    assert_eq!(text_stdout.trim_end(), "text alias transcript");
    assert!(!plain_stdout.contains("schema"));
    assert!(!text_stdout.contains("schema"));
    Ok(())
}

#[test]
fn workspaces_list_json_matches_swift_resource_list_contract()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"{"data":{"items":[{"workspace_id":"workspace-1","title":"Research","preferred_model":"grok-special","custom_personality":"Use citations"},{"id":"workspace-2","name":"Planning","icon":"l:book-open:lime"}]}}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["workspace", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;
    let request = server.recorded_request()?;

    assert_eq!(request.method, "GET");
    assert_eq!(
        request.path,
        "/rest/workspaces?pageSize=50&orderBy=ORDER_BY_LAST_USE_TIME"
    );
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "workspaces");
    assert_eq!(json["subcommand"], "list");
    assert_eq!(json["category"], "resource_list");
    assert_eq!(json["data"]["resource"], "workspace");
    assert_eq!(json["data"]["items"][0]["id"], "workspace-1");
    assert_eq!(json["data"]["items"][0]["workspaceId"], "workspace-1");
    assert_eq!(json["data"]["items"][0]["name"], "Research");
    assert_eq!(json["data"]["items"][0]["title"], "Research");
    assert_eq!(json["data"]["items"][0]["preferredModel"], "grok-special");
    assert_eq!(
        json["data"]["items"][0]["customPersonality"],
        "Use citations"
    );
    assert_eq!(json["data"]["items"][1]["id"], "workspace-2");
    assert_eq!(json["data"]["items"][1]["name"], "Planning");
    assert!(!stdout.contains("Workspaces:"));
    Ok(())
}

#[test]
fn workspaces_create_json_matches_swift_resource_mutation_contract()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"{"workspace":{"workspaceId":"workspace-1","name":"Research","icon":"l:book-open:lime","preferredModel":"grok-special","customPersonality":"Custom instructions"}}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args([
            "workspaces",
            "create",
            "--name",
            "Research",
            "--personality",
            "Custom instructions",
            "--model",
            "grok-special",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;
    let request = server.recorded_request()?;
    let body: Value = serde_json::from_str(&request.body)?;

    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/rest/workspaces");
    assert_eq!(body["name"], "Research");
    assert_eq!(body["icon"], "l:book-open:lime");
    assert_eq!(body["customPersonality"], "Custom instructions");
    assert_eq!(body["preferredModel"], "grok-special");
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "workspaces");
    assert_eq!(json["subcommand"], "create");
    assert_eq!(json["category"], "resource_mutation");
    assert_eq!(json["data"]["resource"], "workspace");
    assert_eq!(json["data"]["action"], "create");
    assert_eq!(json["data"]["id"], "workspace-1");
    assert_eq!(json["data"]["item"]["workspaceId"], "workspace-1");
    assert_eq!(json["data"]["item"]["name"], "Research");
    assert!(!stdout.contains("Created workspace"));
    Ok(())
}

#[test]
fn workspaces_add_and_delete_json_match_swift_mutation_contracts()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"{"workspace":{"workspaceId":"workspace-1","name":"Research"}}"#,
        r#"{"workspace":{"workspaceId":"workspace-1","name":"Research"}}"#,
    ])?;

    let add_output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args([
            "workspaces",
            "add-conversation",
            "workspace-1",
            "conv-1",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let delete_output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["workspaces", "delete", "workspace-1", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let add_stdout = String::from_utf8(add_output)?;
    let delete_stdout = String::from_utf8(delete_output)?;
    let add_json: Value = serde_json::from_str(&add_stdout)?;
    let delete_json: Value = serde_json::from_str(&delete_stdout)?;
    let requests = server.recorded_requests(2)?;
    let add_body: Value = serde_json::from_str(&requests[0].body)?;

    assert_eq!(requests[0].method, "POST");
    assert_eq!(
        requests[0].path,
        "/rest/workspaces/workspace-1/conversations"
    );
    assert_eq!(add_body["conversationId"], "conv-1");
    assert_eq!(add_json["subcommand"], "add-conversation");
    assert_eq!(add_json["category"], "resource_mutation");
    assert_eq!(add_json["data"]["resource"], "workspace");
    assert_eq!(add_json["data"]["action"], "addConversation");
    assert_eq!(add_json["data"]["id"], "workspace-1");
    assert_eq!(add_json["data"]["conversationId"], "conv-1");
    assert_eq!(add_json["data"]["item"]["name"], "Research");
    assert!(!add_stdout.contains("Added conversation"));

    assert_eq!(requests[1].method, "DELETE");
    assert_eq!(requests[1].path, "/rest/workspaces/workspace-1");
    assert_eq!(delete_json["subcommand"], "delete");
    assert_eq!(delete_json["category"], "resource_mutation");
    assert_eq!(delete_json["data"]["resource"], "workspace");
    assert_eq!(delete_json["data"]["action"], "delete");
    assert_eq!(delete_json["data"]["id"], "workspace-1");
    assert_eq!(delete_json["data"]["item"]["workspaceId"], "workspace-1");
    assert!(!delete_stdout.contains("Deleted workspace"));
    Ok(())
}

#[test]
fn workspaces_conversation_json_matches_swift_detail_contract()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"{"conversation":{"conversationId":"conv-1","title":"Research thread","workspaces":["workspace-1","workspace-2"],"taskResult":{"status":"done"}}}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["workspaces", "conversation", "conv-1", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;
    let request = server.recorded_request()?;

    assert_eq!(request.method, "GET");
    assert_eq!(
        request.path,
        "/rest/app-chat/conversations_v2/conv-1?includeWorkspaces=true&includeTaskResult=true"
    );
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "workspaces");
    assert_eq!(json["subcommand"], "conversation");
    assert_eq!(json["category"], "conversation_detail");
    assert_eq!(json["data"]["conversationId"], "conv-1");
    assert_eq!(json["data"]["title"], "Research thread");
    assert_eq!(json["data"]["workspaceCount"], 2);
    assert_eq!(json["data"]["hasTaskResult"], true);
    assert!(!stdout.contains("Conversation:"));
    Ok(())
}

#[test]
fn skills_list_json_matches_swift_resource_list_contract() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"{"skills":[{"skillId":"skill-1","name":"Research","description":"Finds things","status":"active"}]}"#,
        r#"{"userSkills":[{"skill_id":"user-skill-1","title":"My Skill","summary":"Custom skill"}]}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["skills", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;
    let requests = server.recorded_requests(2)?;
    let built_in_body: Value = serde_json::from_str(&requests[0].body)?;

    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/rest/skills");
    assert_eq!(built_in_body["locale"], "en");
    assert_eq!(requests[1].method, "GET");
    assert_eq!(requests[1].path, "/rest/user-skills");
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "skills");
    assert_eq!(json["subcommand"], "list");
    assert_eq!(json["category"], "resource_list");
    assert_eq!(json["data"]["resource"], "skill");
    assert_eq!(json["data"]["scope"], "available");
    assert_eq!(json["data"]["items"][0]["id"], "skill-1");
    assert_eq!(json["data"]["items"][0]["skillId"], "skill-1");
    assert_eq!(json["data"]["items"][0]["name"], "Research");
    assert_eq!(json["data"]["items"][0]["description"], "Finds things");
    assert_eq!(json["data"]["builtInSkills"][0]["status"], "active");
    assert_eq!(json["data"]["userSkills"][0]["id"], "user-skill-1");
    assert_eq!(json["data"]["userSkills"][0]["name"], "My Skill");
    assert!(!stdout.contains("Grok Skills"));
    Ok(())
}

#[test]
fn skills_mine_format_json_matches_swift_user_alias_contract()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"{"userSkills":[{"skillId":"user-skill-1","name":"My Skill","description":"Custom skill"}]}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["skills", "mine", "--format=json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;
    let request = server.recorded_request()?;

    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/rest/user-skills");
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "skills");
    assert_eq!(json["subcommand"], "user");
    assert_eq!(json["category"], "resource_list");
    assert_eq!(json["data"]["resource"], "skill");
    assert_eq!(json["data"]["scope"], "user");
    assert_eq!(json["data"]["items"][0]["id"], "user-skill-1");
    assert_eq!(json["data"]["items"][0]["description"], "Custom skill");
    assert!(!stdout.contains("User Skills"));
    Ok(())
}

#[test]
fn agents_list_json_redacts_instructions_by_default() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"{"agentCustomizations":{"values":[{"agentId":0,"name":"Ignored","instructions":"secret"},{"agent_id":1,"name":"Research","custom_instructions":"Use citations"}]}}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["agents", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;
    let request = server.recorded_request()?;

    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/rest/user-settings");
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "agents");
    assert_eq!(json["subcommand"], "list");
    assert_eq!(json["category"], "resource_list");
    assert_eq!(json["data"]["resource"], "agent");
    assert_eq!(json["data"]["rawRedacted"], true);
    assert_eq!(json["data"]["items"][0]["agentId"], 0);
    assert_eq!(json["data"]["items"][0]["name"], "Grok");
    assert_eq!(json["data"]["items"][0]["instructionLength"], 6);
    assert_eq!(json["data"]["items"][0]["instructionsRedacted"], true);
    assert_eq!(json["data"]["items"][0]["instructions"], Value::Null);
    assert_eq!(json["data"]["items"][1]["agentId"], 1);
    assert_eq!(json["data"]["items"][1]["name"], "Research");
    assert_eq!(json["data"]["items"][1]["instructionLength"], 13);
    assert!(!stdout.contains("Agents:"));
    assert!(!stdout.contains("secret"));
    assert!(!stdout.contains("Use citations"));
    Ok(())
}

#[test]
fn agents_show_json_includes_instructions() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"{"agentCustomizations":{"values":[{"agentId":1,"name":"Research","instructions":"Use citations"}]}}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["agents", "show", "1", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;
    let request = server.recorded_request()?;

    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/rest/user-settings");
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "agents");
    assert_eq!(json["subcommand"], "show");
    assert_eq!(json["category"], "resource_detail");
    assert_eq!(json["data"]["resource"], "agent");
    assert_eq!(json["data"]["item"]["agentId"], 1);
    assert_eq!(json["data"]["item"]["name"], "Research");
    assert_eq!(json["data"]["item"]["instructionsRedacted"], false);
    assert_eq!(json["data"]["item"]["instructions"], "Use citations");
    assert!(!stdout.contains("Agent 1:"));
    Ok(())
}

#[test]
fn agents_view_alias_json_matches_show_contract() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"{"agentCustomizations":{"values":[{"agentId":2,"name":"Coder","instructions":"Write concise patches"}]}}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["agents", "view", "2", "--format=json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;
    let request = server.recorded_request()?;

    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/rest/user-settings");
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "agents");
    assert_eq!(json["subcommand"], "show");
    assert_eq!(json["category"], "resource_detail");
    assert_eq!(json["data"]["resource"], "agent");
    assert_eq!(json["data"]["item"]["agentId"], 2);
    assert_eq!(json["data"]["item"]["name"], "Coder");
    assert_eq!(json["data"]["item"]["instructionsRedacted"], false);
    assert_eq!(
        json["data"]["item"]["instructions"],
        "Write concise patches"
    );
    assert!(!stdout.contains("Agent 2:"));
    Ok(())
}

#[test]
fn agents_clear_json_posts_full_four_agent_profile_with_replace()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(r#"{}"#)?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["agents", "clear", "1", "--replace", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;
    let request = server.recorded_request()?;
    let body: Value = serde_json::from_str(&request.body)?;
    let values = body["agentCustomizations"]["values"]
        .as_array()
        .ok_or_else(|| std::io::Error::other("missing agent values"))?;

    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/rest/user-settings");
    assert_eq!(values.len(), 4);
    assert_eq!(values[0]["agentId"], 0);
    assert_eq!(values[0]["name"], "Grok");
    assert_eq!(values[1]["agentId"], 1);
    assert_eq!(values[1]["name"], "Grok II");
    assert_eq!(values[1]["instructions"], "");
    assert_eq!(values[2]["agentId"], 2);
    assert_eq!(values[3]["agentId"], 3);
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "agents");
    assert_eq!(json["subcommand"], "clear");
    assert_eq!(json["category"], "resource_mutation");
    assert_eq!(json["data"]["resource"], "agent");
    assert_eq!(json["data"]["action"], "clear");
    assert_eq!(json["data"]["id"], "1");
    assert_eq!(json["data"]["rawRedacted"], true);
    assert_eq!(json["data"]["item"]["agentId"], 1);
    assert_eq!(json["data"]["item"]["instructionsRedacted"], true);
    assert!(!stdout.contains("Updated agent"));
    Ok(())
}

#[test]
fn tasks_list_json_matches_swift_resource_list_contract() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"{"tasks":[{"taskId":"task-1","name":"Morning brief","prompt":"Summarize news","isEnabled":true,"schedule":{"timeOfDay":"08:00","timezone":"Asia/Bangkok"}},{"id":"task-2","title":"Paused task","isEnabled":true,"schedule":{"isEnabled":false}}]}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["tasks", "list", "--format=json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;
    let request = server.recorded_request()?;

    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/rest/tasks");
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "tasks");
    assert_eq!(json["subcommand"], "list");
    assert_eq!(json["category"], "resource_list");
    assert_eq!(json["data"]["resource"], "task");
    assert_eq!(json["data"]["items"][0]["id"], "task-1");
    assert_eq!(json["data"]["items"][0]["taskId"], "task-1");
    assert_eq!(json["data"]["items"][0]["name"], "Morning brief");
    assert_eq!(json["data"]["items"][0]["prompt"], "Summarize news");
    assert_eq!(json["data"]["items"][0]["isEnabled"], true);
    assert_eq!(json["data"]["items"][0]["status"], "enabled");
    assert_eq!(json["data"]["items"][0]["schedule"], "08:00 Asia/Bangkok");
    assert_eq!(json["data"]["items"][1]["id"], "task-2");
    assert_eq!(json["data"]["items"][1]["isEnabled"], true);
    assert_eq!(json["data"]["items"][1]["scheduleIsEnabled"], false);
    assert_eq!(json["data"]["items"][1]["status"], "paused");
    assert!(!stdout.contains("Tasks"));
    Ok(())
}

#[test]
fn tasks_select_json_matches_swift_resource_list_contract() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"{"tasks":[{"taskId":"task-select-1","title":"Select daily brief","prompt":"Summarize overnight updates","isEnabled":true,"schedule":{"dayOfYear":"2026-05-15","timeOfDay":"07:00","timezone":"Asia/Bangkok"}},{"taskId":"task-select-2","name":"Select archived check","isEnabled":true,"schedule":{"isEnabled":false}}]}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["tasks", "select", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;
    let request = server.recorded_request()?;

    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/rest/tasks");
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "tasks");
    assert_eq!(json["subcommand"], "select");
    assert_eq!(json["category"], "resource_list");
    assert_eq!(json["data"]["resource"], "task");
    assert_eq!(json["data"]["items"][0]["id"], "task-select-1");
    assert_eq!(json["data"]["items"][0]["taskId"], "task-select-1");
    assert_eq!(json["data"]["items"][0]["name"], "Select daily brief");
    assert_eq!(
        json["data"]["items"][0]["prompt"],
        "Summarize overnight updates"
    );
    assert_eq!(json["data"]["items"][0]["status"], "enabled");
    assert_eq!(
        json["data"]["items"][0]["schedule"],
        "2026-05-15 07:00 Asia/Bangkok"
    );
    assert_eq!(json["data"]["items"][1]["id"], "task-select-2");
    assert_eq!(json["data"]["items"][1]["scheduleIsEnabled"], false);
    assert_eq!(json["data"]["items"][1]["status"], "paused");
    assert!(!stdout.contains("Tasks"));
    Ok(())
}

#[test]
fn tasks_list_human_output_matches_swift_summary_rows() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"{"tasks":[{"taskId":"task-summary-daily","name":"Morning Research Brief","prompt":"Summarize overnight product and AI research updates.","isEnabled":true,"schedule":{"dayOfYear":"2026-05-15","timeOfDay":"08:00","timezone":"America/New_York"}},{"taskId":"task-summary-weekly","name":"Weekly Support Digest","prompt":"Summarize support escalations.","isEnabled":true,"schedule":{"dayOfYear":"2026-05-18","timeOfDay":"09:30","timezone":"UTC"}}]}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["tasks", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let request = server.recorded_request()?;

    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/rest/tasks");
    assert!(stdout.contains("Tasks"));
    assert!(stdout.contains("Morning Research Brief  enabled  2026-05-15 08:00 America/New_York"));
    assert!(stdout.contains("Weekly Support Digest  enabled  2026-05-18 09:30 UTC"));
    assert!(!stdout.contains("task-summary-daily"));
    assert!(!stdout.contains("task-summary-weekly"));
    assert!(!stdout.contains("ID:"));
    assert!(!stdout.contains("Title:"));
    assert!(!stdout.contains("Status:"));
    assert!(!stdout.contains("Schedule:"));
    Ok(())
}

#[test]
fn tasks_results_human_output_matches_swift_latest_run() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"{"tasks":[{"taskId":"task-summary-daily","name":"Morning Research Brief","isEnabled":true}]}"#,
        r#"{"results":[{"taskResultId":"result-latest","taskId":"task-summary-daily","conversationId":"conv-task-summary-daily","responseId":"resp-latest","summary":"Three notable product research updates landed overnight.","state":"TASK_RESULT_DONE","createdAt":"2026-05-15T12:00:00Z"}]}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["tasks", "results", "task-summary-daily"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let requests = server.recorded_requests(2)?;

    assert_eq!(requests[0].path, "/rest/tasks");
    assert_eq!(
        requests[1].path,
        "/rest/tasks/results/task-summary-daily?limit=1"
    );
    assert!(stdout.contains("Latest run  done  2026-05-15 12:00"));
    assert!(stdout.contains("Three notable product research updates landed overnight."));
    assert!(!stdout.contains("task-summary-daily"));
    assert!(!stdout.contains("conv-task-summary-daily"));
    assert!(!stdout.contains("Latest result for"));
    Ok(())
}

#[test]
fn tasks_show_json_loads_latest_result() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"{"tasks":[{"taskId":"task-1","name":"Morning brief","prompt":"Summarize news","isEnabled":true}]}"#,
        r#"{"results":[{"taskResultId":"result-1","taskId":"task-1","conversationId":"conv-1","responseId":"resp-1","summary":"Finished","state":"TASK_RESULT_DONE","createdAt":"2026-05-15T01:02:03Z"}]}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["tasks", "show", "task-1", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;
    let requests = server.recorded_requests(2)?;

    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/rest/tasks");
    assert_eq!(requests[1].method, "GET");
    assert_eq!(requests[1].path, "/rest/tasks/results/task-1?limit=1");
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["command"], "tasks");
    assert_eq!(json["subcommand"], "show");
    assert_eq!(json["category"], "resource_detail");
    assert_eq!(json["data"]["resource"], "task");
    assert_eq!(json["data"]["item"]["id"], "task-1");
    assert_eq!(json["data"]["item"]["title"], "Morning brief");
    assert_eq!(json["data"]["item"]["latestResult"]["id"], "result-1");
    assert_eq!(
        json["data"]["item"]["latestResult"]["conversationId"],
        "conv-1"
    );
    assert_eq!(json["data"]["item"]["latestResult"]["message"], "Finished");
    assert_eq!(
        json["data"]["item"]["latestResult"]["created"],
        "2026-05-15T01:02:03Z"
    );
    assert!(!stdout.contains("Latest run"));
    Ok(())
}

#[test]
fn tasks_chat_json_opens_run_and_sends_message() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"{"tasks":[{"taskId":"task-1","name":"Morning brief","prompt":"Summarize news","isEnabled":true}]}"#,
        r#"{"results":[{"taskResultId":"result-1","taskId":"task-1","conversationId":"conv-1","responseId":"resp-1","summary":"Finished","state":"TASK_RESULT_DONE"}]}"#,
        r#"{"conversation":{"conversationId":"conv-1","title":"Morning brief","taskResult":{"status":"done"}}}"#,
        r#"{"responses":[{"responseId":"resp-1","sender":"assistant","message":"Finished","createTime":"2026-05-15T01:02:03Z"}]}"#,
        r#"data: {"result":{"response":{"modelResponse":{"responseId":"resp-2","message":"Explanation"}}}}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args([
            "tasks",
            "chat",
            "task-1",
            "--message",
            "Explain this",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;
    let requests = server.recorded_requests(5)?;
    let load_body: Value = serde_json::from_str(&requests[3].body)?;
    let continue_body: Value = serde_json::from_str(&requests[4].body)?;

    assert_eq!(requests[0].path, "/rest/tasks");
    assert_eq!(requests[1].path, "/rest/tasks/results/task-1?limit=10");
    assert_eq!(
        requests[2].path,
        "/rest/app-chat/conversations_v2/conv-1?includeWorkspaces=true&includeTaskResult=true"
    );
    assert_eq!(
        requests[3].path,
        "/rest/app-chat/conversations/conv-1/load-responses"
    );
    assert_eq!(load_body, json!({"responseIds": ["resp-1"]}));
    assert_eq!(
        requests[4].path,
        "/rest/app-chat/conversations/conv-1/responses"
    );
    assert_eq!(continue_body["message"], "Explain this");
    assert_eq!(continue_body["parentResponseId"], "resp-1");
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["command"], "tasks");
    assert_eq!(json["subcommand"], "chat");
    assert_eq!(json["category"], "assistant_response");
    assert_eq!(json["data"]["taskId"], "task-1");
    assert_eq!(json["data"]["run"]["result"]["id"], "result-1");
    assert_eq!(json["data"]["conversationId"], "conv-1");
    assert_eq!(json["data"]["parentResponseId"], "resp-1");
    assert_eq!(json["data"]["response"]["message"], "Explanation");
    assert!(!stdout.contains("Opened task run chat"));
    Ok(())
}

#[test]
fn tasks_create_and_archive_json_match_swift_mutation_contracts()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"{"task":{"taskId":"task-created","name":"Coverage","prompt":"check command coverage","isEnabled":true}}"#,
        r#"{"task":{"taskId":"task-created","name":"Coverage","prompt":"check command coverage","isEnabled":false}}"#,
    ])?;

    let create_output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args([
            "tasks",
            "create",
            "--prompt",
            "check command coverage",
            "--name",
            "Coverage",
            "--date",
            "2026-05-15",
            "--time",
            "09:30",
            "--timezone",
            "Asia/Bangkok",
            "--guideline",
            "only notify if useful",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let archive_output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["tasks", "archive", "task-created", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let create_stdout = String::from_utf8(create_output)?;
    let archive_stdout = String::from_utf8(archive_output)?;
    let create_json: Value = serde_json::from_str(&create_stdout)?;
    let archive_json: Value = serde_json::from_str(&archive_stdout)?;
    let requests = server.recorded_requests(2)?;
    let create_body: Value = serde_json::from_str(&requests[0].body)?;
    let archive_body: Value = serde_json::from_str(&requests[1].body)?;

    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/rest/tasks");
    assert_eq!(create_body["name"], "Coverage");
    assert_eq!(create_body["prompt"], "check command coverage");
    assert_eq!(create_body["schedule"]["taskCadence"], "TASK_CADENCE_ONCE");
    assert_eq!(create_body["schedule"]["dayOfYear"], "2026-05-15");
    assert_eq!(create_body["schedule"]["timeOfDay"], "09:30");
    assert_eq!(create_body["schedule"]["timezone"], "Asia/Bangkok");
    assert_eq!(
        create_body["notificationDeciderGuideline"],
        "only notify if useful"
    );
    assert_eq!(create_json["command"], "tasks");
    assert_eq!(create_json["subcommand"], "create");
    assert_eq!(create_json["category"], "resource_mutation");
    assert_eq!(create_json["data"]["resource"], "task");
    assert_eq!(create_json["data"]["action"], "create");
    assert_eq!(create_json["data"]["id"], "task-created");
    assert_eq!(
        create_json["data"]["item"]["prompt"],
        "check command coverage"
    );
    assert!(!create_stdout.contains("Created task"));

    assert_eq!(requests[1].method, "PUT");
    assert_eq!(requests[1].path, "/rest/tasks/archive");
    assert_eq!(archive_body["taskId"], "task-created");
    assert_eq!(archive_body["isEnabled"], false);
    assert_eq!(archive_json["subcommand"], "archive");
    assert_eq!(archive_json["data"]["action"], "archive");
    assert_eq!(archive_json["data"]["id"], "task-created");
    assert_eq!(archive_json["data"]["item"]["isEnabled"], false);
    assert!(!archive_stdout.contains("Archived task"));
    Ok(())
}

#[test]
fn list_json_matches_swift_conversation_list_contract() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(
        r#"{"data":{"items":[{"conversation_id":"conv-1","name":"Research","starred":true,"create_time":"2026-05-14T01:02:03Z","modify_time":"2026-05-14T02:03:04Z","temporary":true,"media_types":["image"],"last_message":"Preview"},{"id":"conv-2","title":"Planning"}]}}"#,
    )?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;
    let request = server.recorded_request()?;

    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/rest/app-chat/conversations?pageSize=100");
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "list");
    assert_eq!(json["subcommand"], Value::Null);
    assert_eq!(json["category"], "conversation_list");
    assert_eq!(json["data"]["conversations"][0]["conversationId"], "conv-1");
    assert_eq!(json["data"]["conversations"][0]["title"], "Research");
    assert_eq!(json["data"]["conversations"][0]["starred"], true);
    assert_eq!(json["data"]["conversations"][0]["temporary"], true);
    assert_eq!(json["data"]["conversations"][0]["mediaTypes"][0], "image");
    assert_eq!(json["data"]["conversations"][1]["conversationId"], "conv-2");
    assert_eq!(json["data"]["conversations"][1]["title"], "Planning");
    assert!(!stdout.contains("Available conversations:"));
    Ok(())
}

#[test]
fn list_conversation_json_matches_swift_history_contract() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn_sequence(vec![
        r#"{"responseNodes":[{"responseId":"resp-1","sender":"human"},{"responseId":"resp-2","sender":"assistant","parentResponseId":"resp-1"}]}"#,
        r#"{"responses":[{"responseId":"resp-1","sender":"human","message":"Hello","createTime":"2026-05-14T01:02:03Z"},{"responseId":"resp-2","sender":"assistant","message":"Hi","createTime":"2026-05-14T01:02:04Z","parentResponseId":"resp-1"}]}"#,
    ])?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["list", "--conversation", "conv-1", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;
    let requests = server.recorded_requests(2)?;
    let load_body: Value = serde_json::from_str(&requests[1].body)?;

    assert_eq!(requests[0].method, "GET");
    assert_eq!(
        requests[0].path,
        "/rest/app-chat/conversations/conv-1/response-node"
    );
    assert_eq!(requests[1].method, "POST");
    assert_eq!(
        requests[1].path,
        "/rest/app-chat/conversations/conv-1/load-responses"
    );
    assert_eq!(load_body, json!({"responseIds": ["resp-1", "resp-2"]}));
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "list");
    assert_eq!(json["subcommand"], "conversation");
    assert_eq!(json["category"], "conversation_history");
    assert_eq!(json["data"]["conversationId"], "conv-1");
    assert_eq!(json["data"]["responses"][0]["responseId"], "resp-1");
    assert_eq!(json["data"]["responses"][0]["sender"], "human");
    assert_eq!(json["data"]["responses"][0]["message"], "Hello");
    assert_eq!(
        json["data"]["responses"][0]["createTime"],
        "2026-05-14T01:02:03Z"
    );
    assert_eq!(json["data"]["responses"][1]["parentResponseId"], "resp-1");
    assert!(!stdout.contains("Loading conversation"));
    Ok(())
}

#[test]
fn list_delete_json_matches_swift_soft_delete_contract() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    std::fs::write(
        temp_dir.path().join("credentials.json"),
        r#"{"sso":"cookie"}"#,
    )?;
    let server = JsonMockServer::spawn(r#"{}"#)?;

    let output = grok()?
        .env("GROK_CONFIG_DIR", temp_dir.path())
        .env("GROK_BASE_URL", &server.base_url)
        .args(["list", "delete", "conv-1", "--yes", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;
    let json: Value = serde_json::from_str(&stdout)?;
    let request = server.recorded_request()?;

    assert_eq!(request.method, "DELETE");
    assert_eq!(request.path, "/rest/app-chat/conversations/soft/conv-1");
    assert_eq!(json["schema"], "grok.cli.result.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "list");
    assert_eq!(json["subcommand"], "delete");
    assert_eq!(json["category"], "conversation_delete");
    assert_eq!(json["data"]["action"], "soft_delete");
    assert_eq!(json["data"]["conversationId"], "conv-1");
    assert_eq!(json["data"]["deleted"], true);
    assert!(!stdout.contains("Deleted conversation"));
    Ok(())
}
