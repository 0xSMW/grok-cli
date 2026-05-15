pub mod cli_display;
pub mod config;
pub mod flag_options;
pub mod fuzzy;
pub mod goal;
pub mod hud;
pub mod input;
mod interactive;
pub mod json_output;
mod model_commands;
pub mod options;
pub mod picker;
pub mod rate_limits;
pub mod router;
pub mod shell;
mod stream_display;
pub mod table;
mod task_format;
pub mod terminal;
pub mod typeahead;

use anyhow::Result;
use base64::{Engine as _, engine::general_purpose};
use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, FixedOffset, Local, NaiveDate, NaiveDateTime,
    Utc,
};
use chrono_tz::Tz;
use clap::{Parser, Subcommand};
use cli_display::{asset_display_name, workspace_display_name};
use flag_options::{apply_model_option, apply_output_format_option};
use goal::{
    GoalCommand as InteractiveGoalCommand, GoalState as InteractiveGoalState, GoalStatus,
    GoalTurnResult, continuation_prompt as continuation_goal_prompt,
    initial_prompt as initial_goal_prompt, parse_command as parse_interactive_goal_command,
    should_continue as should_continue_goal, summary as goal_summary,
    turn_result as goal_turn_result,
};
use grok_client::{
    ConversationResponse, DEFAULT_SPEECH_REFINEMENT_LEVEL, GrokAgentCustomization, GrokAsset,
    GrokAssetListOptions, GrokClient, GrokConversation, GrokConversationListOptions,
    GrokConversationMessage, GrokConversationV2Response, GrokError, GrokFileUploadResponse,
    GrokMessageOptions, GrokMode, GrokShareLinkOptions, GrokSkill, GrokSpeechToTextOptions,
    GrokSpeechToTextResponse, GrokStreamParser, GrokTask, GrokTaskCreateOptions,
    GrokTaskMutationResponse, GrokTaskResult, GrokTaskSchedule, GrokWorkspace,
    GrokWorkspaceCreateOptions, GrokWorkspaceListOptions, infer_audio_format,
};
use interactive::{
    DELETE_CONFIRMATION_PROMPT, is_delete_confirmation, parse_command as parse_interactive_command,
    parse_skill_create_prompt, resolve_output_raw as resolve_interactive_output_raw,
    resolve_toggle as resolve_interactive_toggle,
};
use json_output::{CliJsonResult, default_json_meta, is_json_requested};
use model_commands::{available_models_text, model_set_message};
use rate_limits::{rate_limit_summary, unavailable_rate_limit_summary};
use serde_json::{Map, Value, json};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use stream_display::{GrokStreamMarkupParser, StreamDisplayEvent};
use task_format::{compact_task_status, schedule_enabled, schedule_value, task_status};
use typeahead::{InputTypeaheadSuggestion, RemoteTypeaheadController};

#[derive(Debug, Parser)]
#[command(
    name = "grok",
    version,
    about = "Rust port of the Grok CLI",
    subcommand_negates_reqs = true,
    override_usage = "grok [OPTIONS] [MESSAGE]\n       grok [OPTIONS] <COMMAND> [ARGS]"
)]
pub struct Cli {
    #[command(flatten)]
    pub options: options::GrokCommandOptions,

    #[command(subcommand)]
    pub command: Option<Command>,

    #[arg(trailing_var_arg = true)]
    pub message: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start interactive chat mode.
    Chat {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Send one message and exit.
    Message {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Manage auth credentials.
    Auth {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// List saved conversations.
    List {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show available Grok web modes.
    Models {
        #[command(flatten)]
        options: options::GrokCommandOptions,
    },
    /// Alias for models.
    Modes {
        #[command(flatten)]
        options: options::GrokCommandOptions,
    },
    /// Manage Grok agents.
    Agents {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Manage Grok tasks.
    Tasks {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Manage Grok skills.
    Skills {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Manage Grok workspaces.
    Workspaces {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Alias for workspaces.
    Workspace {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Manage Grok files.
    Files {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Transcribe audio.
    Transcribe {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Test command used by parity harnesses.
    Test {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

pub async fn run_from<I, T>(args: I) -> Result<String>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let raw_args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    if let Some(output) = early_static_command_output(&raw_args)? {
        return Ok(output);
    }
    let normalized_args = normalized_cli_arguments(raw_args);
    if let Some(output) = early_static_command_output(&normalized_args)? {
        return Ok(output);
    }
    let cli = Cli::parse_from(normalized_args);
    run(cli).await
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliRunResult {
    pub output: String,
    pub exit_code: i32,
}

pub async fn run_from_with_status<I, T>(args: I) -> Result<CliRunResult>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let output = run_from(args).await?;
    let exit_code = inferred_exit_code(&output);
    Ok(CliRunResult { output, exit_code })
}

fn inferred_exit_code(output: &str) -> i32 {
    if let Ok(value) = serde_json::from_str::<Value>(output)
        && value["ok"] == false
    {
        return value["error"]["exitCode"]
            .as_i64()
            .and_then(|code| i32::try_from(code).ok())
            .unwrap_or(1);
    }

    let trimmed = output.trim_start();
    if is_plain_usage_error(trimmed) {
        return 2;
    }
    if trimmed.starts_with("Error:") {
        return 1;
    }
    0
}

fn is_plain_usage_error(output: &str) -> bool {
    let normalized = output.to_lowercase();
    output.starts_with("Unknown auth command:")
        || (output.starts_with("Error:")
            && [
                "could not infer audio format",
                "could not read --prompt-file",
                "could not read audio file",
                "inline message arguments",
                "invalid --format value",
                "invalid output format",
                "list delete requires",
                "unknown audio option",
                "please provide a message",
                "please provide a path",
                "requires --yes",
                "requires a file id",
                "requires a format value",
                "requires a model value",
                "requires a path",
                "requires a path or -",
                "requires a value",
                "usage:",
                "use raw or json",
                "is only supported by grok message",
            ]
            .iter()
            .any(|fragment| normalized.contains(fragment)))
}

fn early_static_command_output(raw_args: &[OsString]) -> Result<Option<String>> {
    let Some(arguments) = raw_args
        .iter()
        .skip(1)
        .map(|argument| argument.to_str().map(ToOwned::to_owned))
        .collect::<Option<Vec<_>>>()
    else {
        return Ok(None);
    };
    let Some(command) = arguments.first().map(|argument| argument.to_lowercase()) else {
        return Ok(None);
    };

    if matches!(command.as_str(), "--version" | "-v" | "-V") {
        return Ok(Some("grok-cli".to_string()));
    }

    if router::is_help_argument(&command) {
        if is_json_requested(&arguments) {
            return Ok(Some(help_json()?));
        }
        return Ok(Some(help_text()));
    }

    if router::disabled_top_level_commands().contains(command.as_str()) {
        return Ok(Some(disabled_code_message()));
    }

    let remaining = &arguments[1..];
    if remaining.iter().any(|arg| router::is_help_argument(arg)) {
        return Ok(match command.as_str() {
            "chat" => Some(chat_usage()),
            "message" => Some(message_usage()),
            "transcribe" => Some(transcribe_help_text()),
            "models" | "modes" => Some(models_usage()),
            "test" => Some(test_usage()),
            _ => None,
        });
    }

    Ok(None)
}

fn normalized_cli_arguments(raw_args: Vec<OsString>) -> Vec<OsString> {
    if raw_args.len() < 2 {
        return raw_args;
    }

    let Some(argument_strings) = raw_args
        .iter()
        .skip(1)
        .map(|argument| argument.to_str().map(ToOwned::to_owned))
        .collect::<Option<Vec<_>>>()
    else {
        return raw_args;
    };

    let normalized = router::normalized_top_level_arguments(&argument_strings);
    if normalized == argument_strings {
        return raw_args;
    }

    let mut normalized_args = Vec::with_capacity(raw_args.len());
    normalized_args.push(raw_args[0].clone());
    normalized_args.extend(normalized.into_iter().map(OsString::from));
    normalized_args
}

pub async fn run(cli: Cli) -> Result<String> {
    let _output_format = cli.options.resolved_output_format()?;

    match cli.command {
        Some(Command::Models { options }) => {
            run_models_command("models", options, &cli.options).await
        }
        Some(Command::Modes { options }) => {
            run_models_command("modes", options, &cli.options).await
        }
        Some(Command::Chat { args }) => run_chat_command(args, &cli.options).await,
        Some(Command::Auth { args }) => run_auth_command(args, &cli.options).await,
        Some(Command::Message { args }) => run_message_command(args, &cli.options, "message").await,
        Some(Command::List { args }) => run_list_command(args, &cli.options).await,
        Some(Command::Files { args }) => run_files_command(args, &cli.options).await,
        Some(Command::Agents { args }) => run_agents_command(args, &cli.options).await,
        Some(Command::Tasks { args }) => run_tasks_command(args, &cli.options).await,
        Some(Command::Workspaces { args }) | Some(Command::Workspace { args }) => {
            run_workspaces_command(args, &cli.options).await
        }
        Some(Command::Skills { args }) => run_skills_command(args, &cli.options).await,
        Some(Command::Transcribe { args }) => run_transcribe_command(args, &cli.options).await,
        Some(Command::Test { args }) => run_test_command(args, &cli.options),
        None if cli.message.is_empty() => run_chat_command(Vec::new(), &cli.options).await,
        None => run_message_command(cli.message, &cli.options, "message").await,
    }
}

async fn run_models_command(
    command_name: &str,
    command_options: options::GrokCommandOptions,
    top_level_options: &options::GrokCommandOptions,
) -> Result<String> {
    let effective_options = top_level_options.merged(&command_options);
    let output_format = effective_options.resolved_output_format()?;
    let warnings = effective_options.warnings();
    let modes = load_modes_for_models(effective_options.debug).await;
    let current_mode = GrokMode::resolve_from_modes(Some(&GrokMode::default_mode().id), &modes);

    if !output_format.is_json() {
        return Ok(available_models_text(Some(&current_mode), &modes));
    }

    let data = json!({
        "currentModel": mode_json(&current_mode),
        "models": modes.iter().map(|mode| {
            let mut item = mode_json(mode);
            if let Some(object) = item.as_object_mut() {
                object.insert("selected".to_string(), json!(mode.id == current_mode.id));
            }
            item
        }).collect::<Vec<_>>()
    });
    let result = CliJsonResult::ok(
        command_name,
        None,
        "model_list",
        data,
        default_json_meta(top_level_options.debug, warnings),
    );
    Ok(serde_json::to_string_pretty(&result)?)
}

async fn load_modes_for_models(debug: bool) -> Vec<GrokMode> {
    let Ok(client) = configured_client(debug) else {
        return GrokMode::known_modes();
    };
    match client.list_modes().await {
        Ok(modes) if !modes.is_empty() => modes,
        _ => GrokMode::known_modes(),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedMessageCommand {
    message: String,
    audio_input: Option<ParsedAudioInputOptions>,
    resolved_audio_input: Option<MessageAudioInputMetadata>,
    json: bool,
    raw: bool,
    quiet: bool,
    private_mode: bool,
    stream: bool,
    debug: bool,
    selected_mode: GrokMode,
    file_attachment_ids: Vec<String>,
    file_upload_paths: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Clone, Debug)]
enum MessageCommandOutput {
    Response(ConversationResponse),
    StreamingJson(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedChatCommand {
    initial_message: Option<String>,
    initial_audio: Option<ParsedAudioInputOptions>,
    raw: bool,
    quiet: bool,
    private_mode: bool,
    stream: bool,
    typeahead_enabled: bool,
    debug: bool,
    selected_mode: GrokMode,
    file_attachment_ids: Vec<String>,
    workspace_ids: Vec<String>,
    workspace_label: Option<String>,
    goal: Option<InteractiveGoalState>,
    warnings: Vec<String>,
}

async fn run_chat_command(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
) -> Result<String> {
    if args.len() == 1 && router::is_help_argument(&args[0]) {
        return Ok(chat_usage());
    }
    if top_level_options.json || is_json_requested(&args) {
        return run_message_command(args, top_level_options, "chat").await;
    }

    let mut parsed = match parse_chat_command(args, top_level_options) {
        Ok(parsed) => parsed,
        Err(error) => return Ok(format!("Error: {error}")),
    };

    let live_output = is_interactive_terminal() && !parsed.quiet;
    let mut output = Vec::new();
    let mut conversation_id: Option<String> = None;
    let mut parent_response_id: Option<String> = None;
    output.extend(warning_lines(&parsed.warnings));

    let initial_message = if let Some(audio_input) = parsed.initial_audio.clone() {
        if !parsed.quiet {
            output.push("Transcribing audio...".to_string());
        }
        flush_interactive_output(&mut output, live_output);
        match transcribe_required_audio_input(&audio_input, parsed.debug, parsed.quiet).await {
            Ok((_resolved, response)) => {
                if response.text.trim().is_empty() {
                    output.push("Initial audio did not produce text.".to_string());
                    return Ok(finish_interactive_output(&mut output, live_output));
                }
                Some(response.text)
            }
            Err(error) => {
                output.push(format!("Error: {error}"));
                return Ok(finish_interactive_output(&mut output, live_output));
            }
        }
    } else {
        parsed.initial_message.clone()
    };

    if let Some(initial_message) = initial_message.as_deref() {
        if !parsed.quiet {
            output.push(format!("Sending message: {initial_message}"));
        }
        flush_interactive_output(&mut output, live_output);
        let response = send_interactive_turn(
            initial_message,
            conversation_id.as_deref(),
            parent_response_id.as_deref(),
            &parsed,
            live_output,
        )
        .await;
        match response {
            Ok(response) => {
                conversation_id = Some(response.conversation_id.clone());
                parent_response_id = Some(response.response_id.clone());
                if !interactive_streaming_live(&parsed, live_output) {
                    output.push(response.message);
                }
                parsed.file_attachment_ids.clear();
            }
            Err(error) => {
                output.push(format!("Error: {error}"));
                return Ok(finish_interactive_output(&mut output, live_output));
            }
        }
    } else if !parsed.quiet {
        let service_name = interactive_connected_service_name(parsed.debug, &mut output).await;
        output.push(format!(
            "Connected to {service_name}! Use / for commands, or type help."
        ));
    }

    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        if live_output {
            output.extend(interactive_status_lines(&parsed));
        }
        let Some(line) =
            read_interactive_prompt_line(&mut lines, &mut output, live_output, "> ", &parsed)?
        else {
            break;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if matches!(
            trimmed.to_lowercase().as_str(),
            "/quit" | "/exit" | "quit" | "exit"
        ) {
            if !parsed.quiet {
                output.push("Goodbye!".to_string());
            }
            break;
        }
        if trimmed.eq_ignore_ascii_case("/help") || trimmed.eq_ignore_ascii_case("help") {
            output.push(interactive_help());
            continue;
        }
        if trimmed.eq_ignore_ascii_case("/new") || trimmed.eq_ignore_ascii_case("new") {
            conversation_id = None;
            parent_response_id = None;
            parsed.goal = None;
            output.push("Started a new conversation thread.".to_string());
            continue;
        }
        if let Some(interactive_command) = parse_interactive_command(trimmed) {
            let command = interactive_command.name.as_str();
            if command == "skill" {
                match handle_interactive_skill_create(
                    &interactive_command.remainder,
                    &mut conversation_id,
                    &mut parent_response_id,
                    &mut parsed,
                )
                .await
                {
                    Ok(response) => output.push(response.message),
                    Err(error) => output.push(format!("Error: {error}")),
                }
                continue;
            }

            let args_owned = match interactive_command.arguments() {
                Ok(args) => args,
                Err(error) => {
                    output.push(format!("Error: {error}"));
                    continue;
                }
            };
            let args = args_owned.iter().map(String::as_str).collect::<Vec<_>>();
            match command {
                "share" => {
                    if !args.is_empty() {
                        output.push("Usage: /share".to_string());
                        continue;
                    }
                    match share_current_conversation(
                        conversation_id.as_deref(),
                        parent_response_id.as_deref(),
                        parsed.debug,
                    )
                    .await
                    {
                        Ok(share_link) => match copy_to_clipboard(&share_link) {
                            Ok(()) => output.push(format!("Copied share link {share_link}")),
                            Err(error) => {
                                output.push(format!("Share link {share_link}"));
                                output.push(format!("Clipboard copy failed {error}"));
                            }
                        },
                        Err(error) => output.push(format!("Error: {error}")),
                    }
                }
                "delete" => {
                    let skips_confirmation = args == ["--yes"];
                    if !args.is_empty() && !skips_confirmation {
                        output.push("Usage: /delete [--yes]".to_string());
                        continue;
                    }
                    if !skips_confirmation {
                        if !is_interactive_terminal() || parsed.quiet {
                            output.push("Usage: /delete --yes".to_string());
                            continue;
                        }
                        let confirmation = read_interactive_line(
                            &mut lines,
                            &mut output,
                            live_output,
                            DELETE_CONFIRMATION_PROMPT,
                        )?;
                        if !is_delete_confirmation(confirmation.as_deref()) {
                            output.push("Delete cancelled.".to_string());
                            continue;
                        }
                    }
                    let display_id = conversation_id.clone();
                    match delete_current_conversation(conversation_id.as_deref(), parsed.debug)
                        .await
                    {
                        Ok(()) => {
                            let display_name = display_id.unwrap_or_else(|| "current".to_string());
                            output.push(format!("Deleted conversation {display_name}."));
                            conversation_id = None;
                            parent_response_id = None;
                        }
                        Err(error) => output.push(format!("Error: {error}")),
                    }
                }
                "reason" | "reasoning" => {
                    match resolve_interactive_toggle(true, &args, "/reason") {
                        Ok(_) => output.extend(warning_lines(&[interactive_reasoning_warning()])),
                        Err(error) => output.push(format!("Error: {error}")),
                    }
                }
                "private" => {
                    match resolve_interactive_toggle(parsed.private_mode, &args, "/private") {
                        Ok(next_private) => {
                            let was_private = parsed.private_mode;
                            parsed.private_mode = next_private;
                            if parsed.private_mode && !was_private && conversation_id.is_some() {
                                conversation_id = None;
                                parent_response_id = None;
                                output
                                    .push("Started a new private conversation thread.".to_string());
                            }
                            output.push(format!(
                                "Private mode: {}",
                                enabled_label(parsed.private_mode)
                            ));
                        }
                        Err(error) => output.push(format!("Error: {error}")),
                    }
                }
                "stream" => match resolve_interactive_toggle(parsed.stream, &args, "/stream") {
                    Ok(next_stream) => {
                        parsed.stream = next_stream;
                        output.push(format!("Streaming: {}", enabled_label(parsed.stream)));
                    }
                    Err(error) => output.push(format!("Error: {error}")),
                },
                "typeahead" => {
                    match resolve_interactive_toggle(parsed.typeahead_enabled, &args, "/typeahead")
                    {
                        Ok(next_typeahead) => {
                            parsed.typeahead_enabled = next_typeahead;
                            output.push(format!(
                                "Typeahead: {}",
                                enabled_label(parsed.typeahead_enabled)
                            ));
                        }
                        Err(error) => output.push(format!("Error: {error}")),
                    }
                }
                "format" | "md" | "markdown" | "raw" => {
                    match resolve_interactive_output_raw(command, &args, parsed.raw) {
                        Ok(next_raw) => {
                            parsed.raw = next_raw;
                            output.push(format!(
                                "Output format: {}",
                                output_format_description(parsed.raw)
                            ));
                        }
                        Err(error) => output.push(format!("Error: {error}")),
                    }
                }
                "limits" => {
                    if !args.is_empty() {
                        output.push("Usage: /limits".to_string());
                        continue;
                    }
                    match interactive_rate_limit_summary(&parsed.selected_mode, parsed.debug).await
                    {
                        Ok(summary) => output.push(summary),
                        Err(error) => output.push(format!("Error: {error}")),
                    }
                }
                "goal" => {
                    handle_interactive_goal(
                        &args,
                        &mut conversation_id,
                        &mut parent_response_id,
                        &mut parsed,
                        &mut output,
                    )
                    .await;
                }
                "model" | "models" | "mode" | "modes" => {
                    if args.is_empty() || (args.len() == 1 && args[0].eq_ignore_ascii_case("list"))
                    {
                        output.push(interactive_model_list(&parsed.selected_mode));
                        continue;
                    }
                    if args.len() > 1 {
                        output.push(format!("Usage: /{command} [mode|list]"));
                        continue;
                    }
                    parsed.selected_mode = GrokMode::resolve(Some(args[0]));
                    output.push(model_set_message(&parsed.selected_mode));
                }
                "resume" | "list" => {
                    if !args.is_empty() {
                        output.push("Usage: /resume".to_string());
                        continue;
                    }
                    match select_interactive_conversation(
                        &mut lines,
                        &mut parsed,
                        &mut conversation_id,
                        &mut parent_response_id,
                        &mut output,
                        GrokConversationListOptions::default(),
                        live_output,
                    )
                    .await
                    {
                        Ok(()) => {}
                        Err(error) => output.push(format!("Error: {error}")),
                    }
                }
                "search" => {
                    if args.is_empty() {
                        output.push("Usage: /search <query>".to_string());
                        continue;
                    }
                    match select_interactive_conversation(
                        &mut lines,
                        &mut parsed,
                        &mut conversation_id,
                        &mut parent_response_id,
                        &mut output,
                        GrokConversationListOptions {
                            page_size: 60,
                            search_query: Some(args.join(" ")),
                        },
                        live_output,
                    )
                    .await
                    {
                        Ok(()) => {}
                        Err(error) => output.push(format!("Error: {error}")),
                    }
                }
                "files" => {
                    let command_args = if args.is_empty() {
                        vec!["list".to_string()]
                    } else {
                        interactive_args_to_strings(&args)
                    };
                    push_interactive_command_output(
                        &mut output,
                        run_files_command(command_args, top_level_options).await,
                    );
                }
                "tasks" => {
                    push_interactive_command_output(
                        &mut output,
                        run_tasks_command(interactive_args_to_strings(&args), top_level_options)
                            .await,
                    );
                }
                "skills" => {
                    push_interactive_command_output(
                        &mut output,
                        run_skills_command(interactive_args_to_strings(&args), top_level_options)
                            .await,
                    );
                }
                "agents" => {
                    push_interactive_command_output(
                        &mut output,
                        run_agents_command(interactive_args_to_strings(&args), top_level_options)
                            .await,
                    );
                }
                "auth" => {
                    push_interactive_command_output(
                        &mut output,
                        run_auth_command(interactive_args_to_strings(&args), top_level_options)
                            .await,
                    );
                }
                "workspace" | "workspaces" => {
                    if args.is_empty()
                        || (args.len() == 1 && args[0].eq_ignore_ascii_case("select"))
                    {
                        match select_interactive_workspace(
                            &mut lines,
                            &mut parsed,
                            &mut conversation_id,
                            &mut parent_response_id,
                            &mut output,
                            live_output,
                        )
                        .await
                        {
                            Ok(()) => {}
                            Err(error) => output.push(format!("Error: {error}")),
                        }
                    } else {
                        push_interactive_command_output(
                            &mut output,
                            run_workspaces_command(
                                interactive_args_to_strings(&args),
                                top_level_options,
                            )
                            .await,
                        );
                    }
                }
                "attach" => {
                    match handle_interactive_attach(
                        &args,
                        &mut lines,
                        &mut parsed,
                        &mut output,
                        live_output,
                    )
                    .await
                    {
                        Ok(()) => {}
                        Err(error) => output.push(format!("Error: {error}")),
                    }
                }
                "audio" | "audio-send" => {
                    match handle_interactive_audio(
                        command,
                        &args,
                        &mut lines,
                        &mut conversation_id,
                        &mut parent_response_id,
                        &mut parsed,
                        &mut output,
                        live_output,
                    )
                    .await
                    {
                        Ok(()) => {}
                        Err(error) => output.push(format!("Error: {error}")),
                    }
                }
                "transcribe" => {
                    push_interactive_command_output(
                        &mut output,
                        run_transcribe_command(
                            interactive_args_to_strings(&args),
                            top_level_options,
                        )
                        .await,
                    );
                }
                "clear" | "cls" => clear_interactive_screen(live_output),
                _ => output.extend(unknown_interactive_command_lines(command)),
            }
            continue;
        }

        send_interactive_chat_message(
            trimmed,
            &mut conversation_id,
            &mut parent_response_id,
            &mut parsed,
            &mut output,
            live_output,
        )
        .await;
    }

    Ok(finish_interactive_output(&mut output, live_output))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InteractiveCommandCategory {
    Session,
    Model,
    Files,
    Workspace,
    Library,
    Auth,
    Audio,
    Utility,
}

impl InteractiveCommandCategory {
    const ALL: &'static [Self] = &[
        Self::Session,
        Self::Model,
        Self::Files,
        Self::Workspace,
        Self::Library,
        Self::Auth,
        Self::Audio,
        Self::Utility,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Session => "Session",
            Self::Model => "Model",
            Self::Files => "Files",
            Self::Workspace => "Workspace",
            Self::Library => "Library",
            Self::Auth => "Auth",
            Self::Audio => "Audio",
            Self::Utility => "Utility",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InteractiveCommandSpec {
    command: &'static str,
    aliases: &'static [&'static str],
    usage: &'static str,
    description: &'static str,
    category: InteractiveCommandCategory,
    shows_in_help: bool,
}

const INTERACTIVE_COMMAND_SPECS: &[InteractiveCommandSpec] = &[
    InteractiveCommandSpec {
        command: "/model",
        aliases: &["/mode", "/models", "/modes"],
        usage: "/model [mode|list]",
        description: "Switch the active model",
        category: InteractiveCommandCategory::Model,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/new",
        aliases: &[],
        usage: "/new",
        description: "Start a new conversation thread",
        category: InteractiveCommandCategory::Session,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/help",
        aliases: &[],
        usage: "/help",
        description: "Show interactive command help",
        category: InteractiveCommandCategory::Utility,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/exit",
        aliases: &["/quit"],
        usage: "/exit",
        description: "Exit the app",
        category: InteractiveCommandCategory::Utility,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/resume",
        aliases: &["/list"],
        usage: "/resume",
        description: "Resume a saved conversation",
        category: InteractiveCommandCategory::Session,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/search",
        aliases: &[],
        usage: "/search <query>",
        description: "Search saved conversations",
        category: InteractiveCommandCategory::Session,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/goal",
        aliases: &[],
        usage: "/goal [objective|pause|resume|clear|complete]",
        description: "Run a durable objective until completion",
        category: InteractiveCommandCategory::Session,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/share",
        aliases: &[],
        usage: "/share",
        description: "Copy current conversation share link",
        category: InteractiveCommandCategory::Session,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/limits",
        aliases: &[],
        usage: "/limits",
        description: "Show current rate limits for the active model",
        category: InteractiveCommandCategory::Model,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/stream",
        aliases: &[],
        usage: "/stream [on|off]",
        description: "Toggle streaming responses",
        category: InteractiveCommandCategory::Model,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/typeahead",
        aliases: &[],
        usage: "/typeahead [on|off]",
        description: "Toggle web typeahead suggestions",
        category: InteractiveCommandCategory::Utility,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/format",
        aliases: &["/md", "/markdown", "/raw"],
        usage: "/format [md|raw]",
        description: "Toggle Markdown/Raw output",
        category: InteractiveCommandCategory::Model,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/private",
        aliases: &[],
        usage: "/private [on|off]",
        description: "Toggle private mode",
        category: InteractiveCommandCategory::Session,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/attach",
        aliases: &[],
        usage: "/attach [fileId|list]",
        description: "Browse files and attach one to following messages",
        category: InteractiveCommandCategory::Files,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/attach upload",
        aliases: &[],
        usage: "/attach upload <path>",
        description: "Upload a local file and attach it",
        category: InteractiveCommandCategory::Files,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/attach clear",
        aliases: &[],
        usage: "/attach clear",
        description: "Remove all attached files",
        category: InteractiveCommandCategory::Files,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/audio",
        aliases: &[],
        usage: "/audio [path]",
        description: "Record audio, edit the transcript, then send",
        category: InteractiveCommandCategory::Audio,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/audio file",
        aliases: &[],
        usage: "/audio <path>",
        description: "Transcribe audio, edit the text, then send",
        category: InteractiveCommandCategory::Audio,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/audio send",
        aliases: &["/audio-send"],
        usage: "/audio send <path>",
        description: "Transcribe audio and send immediately",
        category: InteractiveCommandCategory::Audio,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/transcribe",
        aliases: &[],
        usage: "/transcribe <path>",
        description: "Transcribe audio and print the text",
        category: InteractiveCommandCategory::Audio,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/files",
        aliases: &[],
        usage: "/files [list|upload|delete]",
        description: "List or upload assets",
        category: InteractiveCommandCategory::Files,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/workspace",
        aliases: &["/workspaces"],
        usage: "/workspace",
        description: "Choose the project for new chats",
        category: InteractiveCommandCategory::Workspace,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/tasks",
        aliases: &[],
        usage: "/tasks [list|inactive|select|show|results|create|archive]",
        description: "Manage tasks",
        category: InteractiveCommandCategory::Library,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/skills",
        aliases: &[],
        usage: "/skills [list|mine|user]",
        description: "List Grok skills",
        category: InteractiveCommandCategory::Library,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/skill create",
        aliases: &[],
        usage: "/skill create <prompt>",
        description: "Create a Grok skill from a prompt",
        category: InteractiveCommandCategory::Library,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/agents",
        aliases: &[],
        usage: "/agents [list|show|edit|set]",
        description: "Manage agent settings",
        category: InteractiveCommandCategory::Library,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/agents show",
        aliases: &["/agents view"],
        usage: "/agents show <id>",
        description: "Show full agent instructions",
        category: InteractiveCommandCategory::Library,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/agents edit",
        aliases: &[],
        usage: "/agents edit <id>",
        description: "Edit agent instructions",
        category: InteractiveCommandCategory::Library,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/auth",
        aliases: &[],
        usage: "/auth [generate|import|help]",
        description: "Generate or import credentials",
        category: InteractiveCommandCategory::Auth,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/delete",
        aliases: &[],
        usage: "/delete [--yes]",
        description: "Delete current conversation",
        category: InteractiveCommandCategory::Session,
        shows_in_help: true,
    },
    InteractiveCommandSpec {
        command: "/clear",
        aliases: &["/cls"],
        usage: "/clear",
        description: "Clear the screen",
        category: InteractiveCommandCategory::Utility,
        shows_in_help: true,
    },
];

fn unknown_interactive_command_lines(command: &str) -> Vec<String> {
    let mut lines = vec![format!("Unknown command /{command}")];
    if let Some(suggestion) = nearest_interactive_command(command) {
        lines.push(format!("Did you mean {}", suggestion.command));
    }
    lines
}

pub fn interactive_completion_suggestion_displays(
    buffer: &str,
    remote_suggestions: &[InputTypeaheadSuggestion],
) -> Vec<String> {
    interactive_completion_suggestions(buffer, remote_suggestions)
        .into_iter()
        .map(|suggestion| suggestion.display)
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractiveCompletionSuggestion {
    pub display: String,
    pub insert_text: String,
    pub description: String,
    pub requires_argument: bool,
    pub appends_trailing_space: bool,
}

pub fn interactive_completion_suggestions(
    buffer: &str,
    remote_suggestions: &[InputTypeaheadSuggestion],
) -> Vec<InteractiveCompletionSuggestion> {
    if !buffer.starts_with('/') {
        return remote_completion_suggestions(remote_suggestions);
    }

    let normalized_input = buffer.to_lowercase();
    let mut seen = HashSet::new();
    let mut results = Vec::new();

    for spec in INTERACTIVE_COMMAND_SPECS {
        let canonical = spec.command;
        if normalized_input == "/" && !shows_in_empty_slash_menu(spec) {
            continue;
        }
        if normalized_input == "/" && canonical.contains(' ') {
            continue;
        }

        let matches = std::iter::once(canonical)
            .chain(spec.aliases.iter().copied())
            .any(|name| {
                let normalized_name = name.to_lowercase();
                normalized_name.starts_with(&normalized_input)
                    || normalized_input.starts_with(&format!("{normalized_name} "))
            });
        if !matches || !seen.insert(canonical) {
            continue;
        }

        results.push(InteractiveCompletionSuggestion {
            display: canonical.to_string(),
            insert_text: completion_insertion_text(canonical),
            description: spec.description.to_string(),
            requires_argument: spec.usage.contains('<'),
            appends_trailing_space: true,
        });
    }

    results.into_iter().take(32).collect()
}

fn remote_completion_suggestions(
    remote_suggestions: &[InputTypeaheadSuggestion],
) -> Vec<InteractiveCompletionSuggestion> {
    let mut seen = HashSet::new();
    remote_suggestions
        .iter()
        .filter_map(|suggestion| {
            let text = suggestion.insert_text.trim();
            if text.is_empty() || !seen.insert(text.to_lowercase()) {
                return None;
            }
            Some(InteractiveCompletionSuggestion {
                display: suggestion.display.clone(),
                insert_text: suggestion.insert_text.clone(),
                description: suggestion.description.clone(),
                requires_argument: false,
                appends_trailing_space: false,
            })
        })
        .take(32)
        .collect()
}

fn shows_in_empty_slash_menu(spec: &InteractiveCommandSpec) -> bool {
    spec.command != "/stream"
}

fn completion_insertion_text(command: &str) -> String {
    command
        .split_whitespace()
        .take_while(|segment| !segment.starts_with('<'))
        .collect::<Vec<_>>()
        .join(" ")
}

fn nearest_interactive_command(raw_value: &str) -> Option<InteractiveCommandSpec> {
    let query = if raw_value.starts_with('/') {
        raw_value.to_string()
    } else {
        format!("/{raw_value}")
    };

    INTERACTIVE_COMMAND_SPECS
        .iter()
        .flat_map(|spec| {
            std::iter::once(spec.command)
                .chain(spec.aliases.iter().copied())
                .map(move |candidate| (*spec, candidate))
        })
        .filter_map(|(spec, candidate)| fuzzy::score(&query, candidate).map(|score| (spec, score)))
        .max_by(|(lhs_spec, lhs_score), (rhs_spec, rhs_score)| {
            lhs_score
                .cmp(rhs_score)
                .then_with(|| rhs_spec.command.len().cmp(&lhs_spec.command.len()))
        })
        .map(|(spec, _)| spec)
}

fn visible_interactive_command_specs() -> Vec<&'static InteractiveCommandSpec> {
    INTERACTIVE_COMMAND_SPECS
        .iter()
        .filter(|spec| spec.shows_in_help)
        .collect()
}

async fn interactive_connected_service_name(debug: bool, output: &mut Vec<String>) -> String {
    let client = match configured_client(debug) {
        Ok(client) => client,
        Err(error) => {
            if debug {
                output.push(format!("Debug: Could not fetch subscription: {error}"));
            }
            return "Grok".to_string();
        }
    };

    match client.subscriptions_response().await {
        Ok(response) => response.display_name(),
        Err(error) => {
            if debug {
                output.push(format!("Debug: Could not fetch subscription: {error}"));
            }
            "Grok".to_string()
        }
    }
}

fn finish_interactive_output(output: &mut Vec<String>, live_output: bool) -> String {
    flush_interactive_output(output, live_output);
    output.join("\n")
}

fn flush_interactive_output(output: &mut Vec<String>, live_output: bool) {
    if !live_output || output.is_empty() {
        return;
    }

    for line in output.drain(..) {
        println!("{line}");
    }
    let _ = std::io::stdout().flush();
}

fn read_interactive_line(
    lines: &mut std::io::Lines<std::io::StdinLock<'_>>,
    output: &mut Vec<String>,
    live_output: bool,
    prompt: &str,
) -> Result<Option<String>> {
    read_interactive_line_with_prefill(lines, output, live_output, prompt, "")
}

fn read_interactive_line_with_prefill(
    lines: &mut std::io::Lines<std::io::StdinLock<'_>>,
    output: &mut Vec<String>,
    live_output: bool,
    prompt: &str,
    prefill: &str,
) -> Result<Option<String>> {
    flush_interactive_output(output, live_output);
    if live_output {
        match input::read_terminal_line(prompt, prefill, |_| Vec::new())? {
            input::TerminalInputResult::Submitted(line) => return Ok(Some(line)),
            input::TerminalInputResult::Cancelled => return Ok(None),
            input::TerminalInputResult::Unavailable => {}
        }
    }
    if live_output && !prompt.is_empty() {
        print!("{prompt}");
        let _ = std::io::stdout().flush();
        if !prefill.is_empty() {
            print!("{prefill}");
            let _ = std::io::stdout().flush();
        }
    }
    Ok(lines.next().transpose()?)
}

fn read_interactive_prompt_line(
    lines: &mut std::io::Lines<std::io::StdinLock<'_>>,
    output: &mut Vec<String>,
    live_output: bool,
    prompt: &str,
    parsed: &ParsedChatCommand,
) -> Result<Option<String>> {
    flush_interactive_output(output, live_output);
    if live_output {
        let mut remote_cache: std::collections::HashMap<String, Vec<InputTypeaheadSuggestion>> =
            std::collections::HashMap::new();
        let result = input::read_terminal_line(prompt, "", |buffer| {
            let remote_suggestions = if parsed.typeahead_enabled {
                let query = remote_typeahead_query(buffer);
                query
                    .and_then(|query| {
                        if !remote_cache.contains_key(&query) {
                            let suggestions =
                                fetch_remote_typeahead_suggestions(&query, parsed.debug);
                            remote_cache.insert(query.clone(), suggestions);
                        }
                        remote_cache.get(&query).cloned()
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            interactive_completion_suggestions(buffer, &remote_suggestions)
        })?;
        match result {
            input::TerminalInputResult::Submitted(line) => return Ok(Some(line)),
            input::TerminalInputResult::Cancelled => return Ok(None),
            input::TerminalInputResult::Unavailable => {}
        }
    }
    if live_output && !prompt.is_empty() {
        print!("{prompt}");
        let _ = std::io::stdout().flush();
    }
    Ok(lines.next().transpose()?)
}

fn remote_typeahead_query(buffer: &str) -> Option<String> {
    let trimmed = buffer.trim();
    if trimmed.chars().count() < 2
        || trimmed.starts_with('/')
        || trimmed.starts_with("[Pasted content ")
        || trimmed.contains('\n')
        || trimmed.contains('\r')
    {
        return None;
    }
    Some(trimmed.to_string())
}

fn fetch_remote_typeahead_suggestions(query: &str, debug: bool) -> Vec<InputTypeaheadSuggestion> {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return Vec::new();
    };
    let query = query.to_string();
    let result = tokio::task::block_in_place(|| {
        handle.block_on(async move {
            let client = configured_client(debug)?;
            let response = client
                .typeahead_response(&query, "en-US", 3, "web", 2)
                .await?;
            Ok::<_, anyhow::Error>(RemoteTypeaheadController::normalize_remote_suggestions(
                response.suggestions,
                3,
            ))
        })
    });
    result.unwrap_or_default()
}

fn clear_interactive_screen(live_output: bool) {
    if live_output {
        print!("\u{001b}[2J\u{001b}[H");
        let _ = std::io::stdout().flush();
    }
}

fn interactive_status_lines(parsed: &ParsedChatCommand) -> Vec<String> {
    hud::lines(
        &hud::CliHudState {
            model_name: parsed.selected_mode.display_name.clone(),
            workspace_name: parsed.workspace_label.clone(),
            private_mode: parsed.private_mode,
            stream: parsed.stream,
            output_format: if parsed.raw {
                options::OutputFormat::Raw
            } else {
                options::OutputFormat::Markdown
            },
            attached_file_count: parsed.file_attachment_ids.len(),
            rate_limit_warning: None,
        },
        120,
    )
}

async fn send_interactive_chat_message(
    message: &str,
    conversation_id: &mut Option<String>,
    parent_response_id: &mut Option<String>,
    parsed: &mut ParsedChatCommand,
    output: &mut Vec<String>,
    live_output: bool,
) {
    flush_interactive_output(output, live_output);
    let response = send_interactive_turn(
        message,
        conversation_id.as_deref(),
        parent_response_id.as_deref(),
        parsed,
        live_output,
    )
    .await;
    match response {
        Ok(response) => {
            *conversation_id = Some(response.conversation_id.clone());
            *parent_response_id = Some(response.response_id.clone());
            if !interactive_streaming_live(parsed, live_output) {
                output.push(response.message);
            }
            parsed.file_attachment_ids.clear();
        }
        Err(error) => handle_interactive_error(&error, parsed.debug, output),
    }
}

fn handle_interactive_error(error: &anyhow::Error, _debug: bool, output: &mut Vec<String>) {
    output.push(format!("Error: {}", human_error_message(error)));
    if !is_authentication_error(error) {
        return;
    }

    output.push(
        "Authentication failed. Your saved Grok browser cookies may have expired.".to_string(),
    );
    output.push("Trying to refresh credentials from your browser...".to_string());
    match config::run_cookie_extractor(&[], false) {
        Ok(credentials_path) => {
            output.push("Successfully refreshed credentials from browser.".to_string());
            output.push(format!("Saved to: {}", credentials_path.display()));
            output.push("Retry your last message or command.".to_string());
        }
        Err(refresh_error) => {
            output.push(format!(
                "Automatic browser credential refresh failed: {refresh_error}"
            ));
            output.push(
                "Please log in to Grok in your browser, then run 'auth' here or 'grok auth' from your shell."
                    .to_string(),
            );
        }
    }
}

fn human_error_message(error: &anyhow::Error) -> String {
    let raw_message = error.to_string();
    rate_limit_json_message(&raw_message).unwrap_or(raw_message)
}

fn is_authentication_error(error: &anyhow::Error) -> bool {
    if let Some(grok_error) = error.downcast_ref::<GrokError>() {
        return match grok_error {
            GrokError::InvalidCredentials | GrokError::Unauthorized => true,
            GrokError::AccessDenied(_) => false,
            GrokError::Api(message) => message_indicates_authentication_failure(message),
            _ => false,
        };
    }

    message_indicates_authentication_failure(&error.to_string())
}

async fn send_interactive_turn(
    message: &str,
    conversation_id: Option<&str>,
    parent_response_id: Option<&str>,
    parsed: &ParsedChatCommand,
    live_output: bool,
) -> Result<grok_client::ConversationResponse> {
    if interactive_streaming_live(parsed, live_output) {
        stream_interactive_chat_turn(message, conversation_id, parent_response_id, parsed).await
    } else {
        send_chat_turn(message, conversation_id, parent_response_id, parsed).await
    }
}

fn interactive_streaming_live(parsed: &ParsedChatCommand, live_output: bool) -> bool {
    live_output && parsed.stream && !parsed.quiet
}

async fn stream_interactive_chat_turn(
    message: &str,
    conversation_id: Option<&str>,
    parent_response_id: Option<&str>,
    parsed: &ParsedChatCommand,
) -> Result<grok_client::ConversationResponse> {
    let client = configured_client(parsed.debug)?;
    let options = chat_message_options(parsed);
    let request = match conversation_id {
        Some(conversation_id) => client.continue_conversation_request(
            conversation_id,
            parent_response_id,
            message,
            &options,
        )?,
        None => client.new_conversation_request(message, &options)?,
    };

    let mut stream_parser = GrokStreamParser::new(conversation_id.unwrap_or(""));
    let mut answer_parser = GrokStreamMarkupParser::new();
    let mut printed_answer_header = false;
    let mut printed_any_answer = false;
    let mut accumulated_message = String::new();
    let mut latest_response = None;
    let mut final_response = None;

    client
        .stream_request_lines_with_mode(request, Some(&options.mode_id), |line| {
            let Some(response) = stream_parser.consume_line(&line)? else {
                return Ok(());
            };

            if response.is_final {
                final_response = Some(response);
                return Ok(());
            }

            if !(response.is_thinking || response.is_soft_stop && response.message.is_empty()) {
                accumulated_message.push_str(&response.message);
                printed_any_answer |= print_interactive_stream_events(
                    answer_parser.consume(&response.message),
                    &mut printed_answer_header,
                );
            }
            latest_response = Some(response);
            Ok(())
        })
        .await?;

    if final_response.is_none()
        && let Some(response) = stream_parser.finish()
    {
        final_response = Some(response);
    }

    printed_any_answer |=
        print_interactive_stream_events(answer_parser.finish(), &mut printed_answer_header);

    if let Some(response) = final_response {
        if !printed_any_answer {
            print_interactive_response_body(&response.message, &mut printed_answer_header);
        }
        finish_interactive_stream_response(printed_answer_header);
        return Ok(response);
    }

    if let Some(response) = latest_response {
        let response = grok_client::ConversationResponse::final_message(
            accumulated_message.trim().to_string(),
            response.conversation_id,
            response.response_id,
            None,
            None,
            false,
        );
        if !printed_any_answer {
            print_interactive_response_body(&response.message, &mut printed_answer_header);
        }
        finish_interactive_stream_response(printed_answer_header);
        return Ok(response);
    }

    anyhow::bail!("Could not read Grok streaming response")
}

fn print_interactive_stream_events(
    events: Vec<StreamDisplayEvent>,
    printed_answer_header: &mut bool,
) -> bool {
    let mut printed_text = false;
    for event in events {
        match event {
            StreamDisplayEvent::Text(text) if !text.is_empty() => {
                print_interactive_answer_header(printed_answer_header);
                print!("{text}");
                printed_text = true;
            }
            StreamDisplayEvent::Text(_) | StreamDisplayEvent::Activity(_) => {}
        }
    }
    if printed_text {
        let _ = std::io::stdout().flush();
    }
    printed_text
}

fn print_interactive_response_body(message: &str, printed_answer_header: &mut bool) {
    let visible = GrokStreamMarkupParser::visible_text(message, true);
    if visible.is_empty() {
        return;
    }
    print_interactive_answer_header(printed_answer_header);
    print!("{visible}");
    let _ = std::io::stdout().flush();
}

fn print_interactive_answer_header(printed_answer_header: &mut bool) {
    if !*printed_answer_header {
        println!("\nGrok");
        *printed_answer_header = true;
    }
}

fn finish_interactive_stream_response(printed_answer_header: bool) {
    if printed_answer_header {
        println!("\n");
        let _ = std::io::stdout().flush();
    }
}

fn message_indicates_authentication_failure(message: &str) -> bool {
    let normalized = message.to_lowercase();
    normalized.contains("http error: 401")
        || normalized.contains("unauthorized")
        || normalized.contains("unauthenticated")
        || normalized.contains("not authenticated")
        || normalized.contains("authentication required")
        || normalized.contains("login required")
        || normalized.contains("log in")
        || (normalized.contains("cookie")
            && (normalized.contains("invalid") || normalized.contains("expired")))
        || normalized.contains("csrf")
        || normalized.contains("sso")
}

async fn select_interactive_conversation(
    lines: &mut std::io::Lines<std::io::StdinLock<'_>>,
    parsed: &mut ParsedChatCommand,
    conversation_id: &mut Option<String>,
    parent_response_id: &mut Option<String>,
    output: &mut Vec<String>,
    list_options: GrokConversationListOptions,
    live_output: bool,
) -> Result<()> {
    let client = configured_client(parsed.debug)?;
    let response = client.list_conversations_response(&list_options).await?;
    let conversations = response.conversations;
    if conversations.is_empty() {
        output.push("No conversations found.".to_string());
        return Ok(());
    }

    let selected_index = if live_output {
        flush_interactive_output(output, live_output);
        let items = conversation_picker_items(&conversations);
        match picker::select_index_from_terminal("Select conversation", &items, None)? {
            picker::ArrowSelection::Selected(index) => Some(index),
            picker::ArrowSelection::Cancelled => None,
            picker::ArrowSelection::Unavailable => select_interactive_numbered_index(
                lines,
                output,
                live_output,
                conversation_rows(&conversations),
                "Select a conversation by number: ",
                conversations.len(),
                false,
            )?,
        }
    } else {
        select_interactive_numbered_index(
            lines,
            output,
            live_output,
            conversation_rows(&conversations),
            "Select a conversation by number: ",
            conversations.len(),
            false,
        )?
    };

    let Some(selected_index) = selected_index else {
        return Ok(());
    };
    let selected = &conversations[selected_index];
    output.push(format!("Loading conversation \"{}\"...", selected.title));
    let responses = client
        .load_responses(&selected.conversation_id, None)
        .await?;
    *conversation_id = Some(selected.conversation_id.clone());
    *parent_response_id = continuation_parent_response_id(&responses);
    if let Some(mode) = most_recent_response_mode(&responses) {
        parsed.selected_mode = mode;
        output.push(format!(
            "Model: {} ({})",
            parsed.selected_mode.display_name, parsed.selected_mode.id
        ));
    }

    output.push(selected.title.clone());
    output.push(conversation_history_rows(&responses));
    Ok(())
}

async fn select_interactive_workspace(
    lines: &mut std::io::Lines<std::io::StdinLock<'_>>,
    parsed: &mut ParsedChatCommand,
    conversation_id: &mut Option<String>,
    parent_response_id: &mut Option<String>,
    output: &mut Vec<String>,
    live_output: bool,
) -> Result<()> {
    let client = configured_client(parsed.debug)?;
    let response = client
        .list_workspaces_response(&GrokWorkspaceListOptions::default())
        .await?;
    let workspaces = response.workspaces;
    if workspaces.is_empty() {
        output.push("No workspaces found.".to_string());
        return Ok(());
    }

    if let Some(label) = parsed.workspace_label.as_deref() {
        output.push(format!("Current workspace: {label}"));
    }
    let selected_index = if live_output {
        flush_interactive_output(output, live_output);
        let items = workspace_picker_items(&workspaces);
        match picker::select_index_from_terminal(
            "Select workspace",
            &items,
            parsed.workspace_ids.first().map(String::as_str),
        )? {
            picker::ArrowSelection::Selected(index) => Some(index),
            picker::ArrowSelection::Cancelled => None,
            picker::ArrowSelection::Unavailable => select_interactive_numbered_index(
                lines,
                output,
                live_output,
                interactive_workspace_selection_rows(&workspaces),
                "Select workspace by number: ",
                workspaces.len(),
                true,
            )?,
        }
    } else {
        select_interactive_numbered_index(
            lines,
            output,
            live_output,
            interactive_workspace_selection_rows(&workspaces),
            "Select workspace by number: ",
            workspaces.len(),
            true,
        )?
    };

    let Some(selected_index) = selected_index else {
        return Ok(());
    };

    conversation_id.take();
    parent_response_id.take();
    if selected_index == 0 {
        parsed.workspace_ids.clear();
        parsed.workspace_label = None;
        output.push("Workspace cleared. New chats will not be project-scoped.".to_string());
        return Ok(());
    }

    let workspace = &workspaces[selected_index - 1];
    let Some(workspace_id) = workspace.resolved_id().map(ToOwned::to_owned) else {
        output.push("Selected workspace has no usable ID.".to_string());
        return Ok(());
    };
    parsed.workspace_ids = vec![workspace_id];
    let label = workspace_display_name(workspace);
    parsed.workspace_label = Some(label.clone());
    output.push(format!("Workspace set to: {label}"));
    Ok(())
}

fn interactive_workspace_selection_rows(workspaces: &[GrokWorkspace]) -> String {
    let mut lines = vec!["Select workspace:".to_string(), "0. None".to_string()];
    for (index, workspace) in workspaces.iter().enumerate() {
        let label = workspace_display_name(workspace);
        let id = workspace.resolved_id().unwrap_or("");
        lines.push(format!("{}. {label} {id}", index + 1));
    }
    lines.join("\n")
}

fn select_interactive_numbered_index(
    lines: &mut std::io::Lines<std::io::StdinLock<'_>>,
    output: &mut Vec<String>,
    live_output: bool,
    rows: String,
    prompt: &str,
    item_count: usize,
    allows_zero: bool,
) -> Result<Option<usize>> {
    output.push(rows);
    let Some(selection) = read_interactive_line(lines, output, live_output, prompt)? else {
        return Ok(None);
    };
    let selection = selection.trim();
    let Ok(number) = selection.parse::<usize>() else {
        output.push("Invalid selection.".to_string());
        return Ok(None);
    };

    if allows_zero && number == 0 {
        return Ok(Some(0));
    }
    if number == 0 || number > item_count {
        output.push("Invalid selection.".to_string());
        return Ok(None);
    }
    Ok(Some(if allows_zero { number } else { number - 1 }))
}

fn conversation_picker_items(conversations: &[GrokConversation]) -> Vec<picker::PickerItem> {
    conversations
        .iter()
        .map(|conversation| {
            let subtitle = if conversation.modify_time.is_empty() {
                conversation.conversation_id.clone()
            } else {
                format!("modified {}", conversation.modify_time)
            };
            let mut item =
                picker::PickerItem::new(&conversation.conversation_id, &conversation.title);
            item.subtitle = Some(subtitle);
            if !conversation.modify_time.is_empty() {
                item.metadata_label = Some("modified".to_string());
                item.metadata = Some(conversation.modify_time.clone());
            }
            if !conversation.preview.is_empty() {
                item.preview = Some(conversation.preview.clone());
            }
            item.rebuild_search_text();
            item
        })
        .collect()
}

fn workspace_picker_items(workspaces: &[GrokWorkspace]) -> Vec<picker::PickerItem> {
    let mut items = vec![picker::PickerItem {
        subtitle: Some("New chats will not be project-scoped".to_string()),
        preview_label: "scope".to_string(),
        preview: Some("Clear the current workspace selection.".to_string()),
        ..picker::PickerItem::new("__none__", "None")
    }];

    items.extend(workspaces.iter().enumerate().map(|(index, workspace)| {
        let id = workspace
            .resolved_id()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("workspace-{index}"));
        let mut item = picker::PickerItem::new(id, workspace_display_name(workspace));
        item.subtitle = workspace.resolved_id().map(ToOwned::to_owned);
        item.metadata_label = Some("model".to_string());
        item.metadata = workspace.preferred_model.clone();
        item.preview_label = "personality".to_string();
        item.preview = workspace.custom_personality.clone();
        item.is_enabled = workspace.resolved_id().is_some();
        item.rebuild_search_text();
        item
    }));
    items
}

fn asset_picker_items(assets: &[GrokAsset]) -> Vec<picker::PickerItem> {
    assets
        .iter()
        .enumerate()
        .map(|(index, asset)| {
            let id = asset
                .resolved_id()
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| format!("asset-{index}"));
            let mut item = picker::PickerItem::new(id, asset_display_name(asset));
            item.subtitle = asset.resolved_id().map(ToOwned::to_owned);
            item.metadata_label = Some("mime".to_string());
            item.metadata = asset.mime_type.clone();
            item.is_enabled = asset.resolved_id().is_some();
            item.rebuild_search_text();
            item
        })
        .collect()
}

async fn handle_interactive_attach(
    args: &[&str],
    lines: &mut std::io::Lines<std::io::StdinLock<'_>>,
    parsed: &mut ParsedChatCommand,
    output: &mut Vec<String>,
    live_output: bool,
) -> Result<()> {
    let command = parsed_attach_command(args, lines, parsed.debug, output, live_output).await?;
    match command {
        AttachCommand::Select(file_id, label) => {
            append_unique(file_id.clone(), &mut parsed.file_attachment_ids);
            output.push(format!("Attached: {label}"));
        }
        AttachCommand::AttachId(file_id) => {
            append_unique(file_id.clone(), &mut parsed.file_attachment_ids);
            output.push(format!("Attached file ID: {file_id}"));
        }
        AttachCommand::Upload(path) => {
            let client = configured_client(parsed.debug)?;
            let upload_path = expand_tilde_path(PathBuf::from(&path));
            let data = std::fs::read(&upload_path)?;
            let file_name = file_name_for_upload(&upload_path)?;
            let mime_type = inferred_file_mime_type(&upload_path);
            let content_base64 = general_purpose::STANDARD.encode(data);
            let response = client
                .upload_file_response(&file_name, &mime_type, &content_base64)
                .await?;
            let file_id = response
                .uploaded_file_id()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("Uploaded file response did not include an attachment ID")
                })?
                .to_string();
            append_unique(file_id.clone(), &mut parsed.file_attachment_ids);
            output.push(format!(
                "Uploaded and attached: {}",
                response.file_name.as_deref().unwrap_or(&file_name)
            ));
            output.push(format!("File ID: {file_id}"));
        }
        AttachCommand::Clear => {
            parsed.file_attachment_ids.clear();
            output.push("Cleared attached files.".to_string());
        }
        AttachCommand::Noop => {}
    }
    Ok(())
}

async fn parsed_attach_command(
    args: &[&str],
    lines: &mut std::io::Lines<std::io::StdinLock<'_>>,
    debug: bool,
    output: &mut Vec<String>,
    live_output: bool,
) -> Result<AttachCommand> {
    if args.is_empty() || (args.len() == 1 && args[0].eq_ignore_ascii_case("list")) {
        return select_interactive_attachment(lines, debug, output, live_output).await;
    }
    if args[0].eq_ignore_ascii_case("clear") {
        if args.len() != 1 {
            anyhow::bail!("Usage: /attach clear");
        }
        return Ok(AttachCommand::Clear);
    }
    if args[0].eq_ignore_ascii_case("upload") {
        if args.len() != 2 || args[1].trim().is_empty() {
            anyhow::bail!("Usage: /attach upload <path>");
        }
        return Ok(AttachCommand::Upload(args[1].to_string()));
    }
    if args.len() != 1 {
        anyhow::bail!("Usage: /attach <fileId>");
    }
    Ok(AttachCommand::AttachId(args[0].to_string()))
}

async fn select_interactive_attachment(
    lines: &mut std::io::Lines<std::io::StdinLock<'_>>,
    debug: bool,
    output: &mut Vec<String>,
    live_output: bool,
) -> Result<AttachCommand> {
    let client = configured_client(debug)?;
    let response = client
        .list_assets_response(&GrokAssetListOptions {
            page_size: 25,
            ..GrokAssetListOptions::default()
        })
        .await?;
    let assets = response.assets;
    if assets.is_empty() {
        output.push("No files found.".to_string());
        return Ok(AttachCommand::Noop);
    }

    let selected_index = if live_output {
        flush_interactive_output(output, live_output);
        let items = asset_picker_items(&assets);
        match picker::select_index_from_terminal("Select file", &items, None)? {
            picker::ArrowSelection::Selected(index) => Some(index),
            picker::ArrowSelection::Cancelled => None,
            picker::ArrowSelection::Unavailable => select_interactive_numbered_index(
                lines,
                output,
                live_output,
                interactive_attachment_selection_rows(&assets),
                "Select file by number: ",
                assets.len(),
                false,
            )?,
        }
    } else {
        select_interactive_numbered_index(
            lines,
            output,
            live_output,
            interactive_attachment_selection_rows(&assets),
            "Select file by number: ",
            assets.len(),
            false,
        )?
    };

    let Some(selected_index) = selected_index else {
        return Ok(AttachCommand::Noop);
    };
    let asset = &assets[selected_index];
    let Some(file_id) = asset.resolved_id().map(ToOwned::to_owned) else {
        output.push("Selected file has no usable attachment ID.".to_string());
        return Ok(AttachCommand::Noop);
    };
    let label = asset_display_name(asset);
    Ok(AttachCommand::Select(file_id, label))
}

fn interactive_attachment_selection_rows(assets: &[GrokAsset]) -> String {
    let mut lines = vec!["Select file to attach:".to_string()];
    for (index, asset) in assets.iter().enumerate() {
        let label = asset_display_name(asset);
        let id = asset.resolved_id().unwrap_or("");
        lines.push(format!("{}. {label} {id}", index + 1));
    }
    lines.join("\n")
}

#[derive(Debug)]
enum AttachCommand {
    Select(String, String),
    AttachId(String),
    Upload(String),
    Clear,
    Noop,
}

#[allow(clippy::too_many_arguments)]
async fn handle_interactive_audio(
    command: &str,
    args: &[&str],
    lines: &mut std::io::Lines<std::io::StdinLock<'_>>,
    conversation_id: &mut Option<String>,
    parent_response_id: &mut Option<String>,
    parsed: &mut ParsedChatCommand,
    output: &mut Vec<String>,
    live_output: bool,
) -> Result<()> {
    let audio = parse_interactive_audio_command(command, args, parsed)?;
    if audio.input.path.as_deref() == Some("-") {
        anyhow::bail!(
            "Audio stdin is only supported by non-interactive message/transcribe commands"
        );
    }

    if !parsed.quiet {
        if audio.input.path.is_none() {
            output.push("Recording audio...".to_string());
        } else {
            output.push("Transcribing audio...".to_string());
        }
    }
    let (_resolved, response) = transcribe_interactive_audio_input(
        &audio.input,
        lines,
        parsed.quiet,
        output,
        parsed.debug,
        live_output,
    )
    .await?;
    if !parsed.quiet {
        output.push(interactive_audio_transcript(&response.text));
    }

    let message = if audio.send_immediately {
        response.text
    } else {
        if !parsed.quiet {
            output.push("Edit transcript, then press Enter to send.".to_string());
        }
        if is_interactive_terminal() {
            let Some(edited) = read_interactive_line_with_prefill(
                lines,
                output,
                live_output,
                "> ",
                &response.text,
            )?
            else {
                return Ok(());
            };
            let edited = edited.trim();
            if edited.is_empty() {
                return Ok(());
            }
            edited.to_string()
        } else {
            response.text
        }
    };

    if message.trim().is_empty() {
        output.push("Initial audio did not produce text.".to_string());
        return Ok(());
    }
    send_interactive_chat_message(
        &message,
        conversation_id,
        parent_response_id,
        parsed,
        output,
        live_output,
    )
    .await;
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InteractiveAudioCommand {
    send_immediately: bool,
    input: ParsedAudioInputOptions,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedAudioInputOptions {
    path: Option<String>,
    audio_format: Option<String>,
    refinement_level: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MessageAudioInputMetadata {
    transcript: String,
    source_path: String,
    audio_format: String,
    refinement_level: String,
}

fn parse_interactive_audio_command(
    command: &str,
    args: &[&str],
    _parsed: &ParsedChatCommand,
) -> Result<InteractiveAudioCommand> {
    let mut audio_args = interactive_args_to_strings(args);
    let mut send_immediately = command == "audio-send";
    let mut recording_requested = false;

    if let Some(first) = audio_args.first().map(|value| value.to_lowercase()) {
        match first.as_str() {
            "send" => {
                send_immediately = true;
                audio_args.remove(0);
            }
            "file" => {
                audio_args.remove(0);
            }
            "record" => {
                recording_requested = true;
                audio_args.remove(0);
            }
            _ => {}
        }
    }

    let usage =
        "/audio [send|file|record] [--audio-format <format>] [--refinement-level <level>] [path]";
    if recording_requested && audio_args.iter().any(|arg| !arg.starts_with("--")) {
        anyhow::bail!("Usage: {usage}");
    }

    let input = parse_optional_audio_input_options(&audio_args, usage)?;
    Ok(InteractiveAudioCommand {
        send_immediately,
        input,
    })
}

async fn transcribe_interactive_audio_input(
    input: &ParsedAudioInputOptions,
    lines: &mut std::io::Lines<std::io::StdinLock<'_>>,
    quiet: bool,
    output: &mut Vec<String>,
    debug: bool,
    live_output: bool,
) -> Result<(ResolvedAudioInput, GrokSpeechToTextResponse)> {
    let parsed = if let Some(path) = input.path.clone() {
        ParsedTranscribeCommand {
            path,
            audio_format: input.audio_format.clone(),
            refinement_level: input.refinement_level.clone(),
            json: false,
            quiet,
            debug,
        }
    } else {
        if let Some(audio_format) = input.audio_format.as_deref()
            && audio_format.trim().to_lowercase() != "webm"
        {
            anyhow::bail!(
                "Recording creates webm audio; omit --audio-format or use --audio-format webm."
            );
        }
        let path = record_audio_to_temporary_webm(lines, quiet, output, live_output)?;
        ParsedTranscribeCommand {
            path,
            audio_format: Some("webm".to_string()),
            refinement_level: input.refinement_level.clone(),
            json: false,
            quiet,
            debug,
        }
    };

    transcribe_audio_input(&parsed).await
}

async fn transcribe_required_audio_input(
    input: &ParsedAudioInputOptions,
    debug: bool,
    quiet: bool,
) -> Result<(ResolvedAudioInput, GrokSpeechToTextResponse)> {
    let path = input
        .path
        .clone()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("--audio requires a path or -"))?;
    let parsed = ParsedTranscribeCommand {
        path,
        audio_format: input.audio_format.clone(),
        refinement_level: input.refinement_level.clone(),
        json: false,
        quiet,
        debug,
    };
    transcribe_audio_input(&parsed).await
}

fn record_audio_to_temporary_webm(
    lines: &mut std::io::Lines<std::io::StdinLock<'_>>,
    quiet: bool,
    output: &mut Vec<String>,
    live_output: bool,
) -> Result<String> {
    let output_path = temporary_audio_recording_path();
    if let Ok(fixture_path) = std::env::var("GROK_CLI_AUDIO_RECORD_FIXTURE")
        && !fixture_path.trim().is_empty()
    {
        let expanded_fixture = expand_tilde_path(PathBuf::from(fixture_path.trim()));
        std::fs::copy(&expanded_fixture, &output_path).map_err(|error| {
            anyhow::anyhow!(
                "Could not use recording fixture {}: {error}",
                fixture_path.trim()
            )
        })?;
        return Ok(output_path.to_string_lossy().to_string());
    }

    #[cfg(target_os = "macos")]
    {
        use std::process::Stdio;

        let ffmpeg_path = find_executable_in_path("ffmpeg").ok_or_else(|| {
            anyhow::anyhow!(
                "Recording from /audio requires ffmpeg on macOS. Install it with `brew install ffmpeg`, then run /audio again."
            )
        })?;
        let device = preferred_avfoundation_audio_device_specifier();
        let input = avfoundation_audio_input_argument(&device);
        let mut child = ProcessCommand::new(ffmpeg_path)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-f",
                "avfoundation",
                "-i",
                &input,
                "-vn",
                "-ac",
                "1",
                "-ar",
                "48000",
                "-c:a",
                "libopus",
                "-b:a",
                "64k",
                "-f",
                "webm",
                "-y",
            ])
            .arg(&output_path)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| anyhow::anyhow!("Could not start audio recording: {error}"))?;

        let prompt = if quiet {
            ""
        } else {
            "Press Enter to stop recording... "
        };
        let _ = read_interactive_line(lines, output, live_output, prompt)?;
        let child_id = child.id();
        if child.try_wait()?.is_none() && !interrupt_process(child_id) {
            let _ = child.kill();
        }
        let process_output = child.wait_with_output()?;
        if std::fs::metadata(&output_path)
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false)
        {
            return Ok(output_path.to_string_lossy().to_string());
        }

        let stderr = String::from_utf8_lossy(&process_output.stderr)
            .trim()
            .to_string();
        if stderr.is_empty() {
            anyhow::bail!("Audio recording failed.");
        }
        anyhow::bail!("Audio recording failed: {stderr}");
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (lines, quiet, output, live_output);
        anyhow::bail!(
            "Recording from /audio is currently supported on macOS with ffmpeg installed."
        );
    }
}

fn temporary_audio_recording_path() -> PathBuf {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!(
        "grok-audio-{}-{timestamp}.webm",
        std::process::id()
    ))
}

#[cfg(target_os = "macos")]
fn find_executable_in_path(executable_name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path_value| {
        std::env::split_paths(&path_value)
            .map(|directory| directory.join(executable_name))
            .find(|candidate| {
                candidate
                    .metadata()
                    .map(|metadata| metadata.is_file())
                    .unwrap_or(false)
            })
    })
}

#[cfg(target_os = "macos")]
fn preferred_avfoundation_audio_device_specifier() -> String {
    if let Some(override_device) = std::env::var("GROK_CLI_AUDIO_DEVICE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return override_device;
    }

    if let Some(device_name) = default_audio_input_device_name()
        && let Some(specifier) = avfoundation_specifier_for_default_audio_device_name(&device_name)
    {
        return specifier;
    }

    "default".to_string()
}

#[cfg(any(target_os = "macos", test))]
fn avfoundation_specifier_for_default_audio_device_name(device_name: &str) -> Option<String> {
    let trimmed = device_name.trim();
    if trimmed.is_empty()
        || trimmed.contains(':')
        || trimmed
            .chars()
            .next()
            .map(|character| character.is_numeric())
            .unwrap_or(false)
    {
        return None;
    }

    Some(trimmed.to_string())
}

#[cfg(any(target_os = "macos", test))]
fn avfoundation_audio_input_argument(device_specifier: &str) -> String {
    let trimmed = device_specifier.trim();
    if trimmed.starts_with(':') {
        trimmed.to_string()
    } else {
        format!(
            ":{}",
            if trimmed.is_empty() {
                "default"
            } else {
                trimmed
            }
        )
    }
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct AudioObjectPropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

#[cfg(target_os = "macos")]
#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyData(
        object_id: u32,
        address: *const AudioObjectPropertyAddress,
        qualifier_data_size: u32,
        qualifier_data: *const std::ffi::c_void,
        data_size: *mut u32,
        data: *mut std::ffi::c_void,
    ) -> i32;
}

#[cfg(target_os = "macos")]
fn default_audio_input_device_name() -> Option<String> {
    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};
    use std::ffi::c_void;

    const NO_ERR: i32 = 0;
    const AUDIO_OBJECT_SYSTEM_OBJECT: u32 = 1;
    const AUDIO_OBJECT_UNKNOWN: u32 = 0;
    const AUDIO_HARDWARE_PROPERTY_DEFAULT_INPUT_DEVICE: u32 = 0x6449_6e20;
    const AUDIO_OBJECT_PROPERTY_NAME: u32 = 0x6c6e_616d;
    const AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL: u32 = 0x676c_6f62;
    const AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN: u32 = 0;

    let default_input_address = AudioObjectPropertyAddress {
        selector: AUDIO_HARDWARE_PROPERTY_DEFAULT_INPUT_DEVICE,
        scope: AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
        element: AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
    };
    let mut device_id = AUDIO_OBJECT_UNKNOWN;
    let mut device_id_size = std::mem::size_of_val(&device_id) as u32;
    let device_status = unsafe {
        AudioObjectGetPropertyData(
            AUDIO_OBJECT_SYSTEM_OBJECT,
            &default_input_address,
            0,
            std::ptr::null(),
            &mut device_id_size,
            (&mut device_id as *mut u32).cast::<c_void>(),
        )
    };
    if device_status != NO_ERR || device_id == AUDIO_OBJECT_UNKNOWN {
        return None;
    }

    let name_address = AudioObjectPropertyAddress {
        selector: AUDIO_OBJECT_PROPERTY_NAME,
        scope: AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
        element: AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
    };
    let mut name: CFStringRef = std::ptr::null();
    let mut name_size = std::mem::size_of::<CFStringRef>() as u32;
    let name_status = unsafe {
        AudioObjectGetPropertyData(
            device_id,
            &name_address,
            0,
            std::ptr::null(),
            &mut name_size,
            (&mut name as *mut CFStringRef).cast::<c_void>(),
        )
    };
    if name_status != NO_ERR || name.is_null() {
        return None;
    }

    let device_name = unsafe { CFString::wrap_under_get_rule(name) }.to_string();
    let trimmed = device_name.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(target_os = "macos")]
fn interrupt_process(pid: u32) -> bool {
    ProcessCommand::new("kill")
        .arg("-INT")
        .arg(pid.to_string())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn is_interactive_terminal() -> bool {
    #[cfg(debug_assertions)]
    if std::env::var_os("GROK_CLI_FORCE_INTERACTIVE_TTY").is_some() {
        return true;
    }
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

fn parse_optional_audio_input_options(
    args: &[String],
    usage: &str,
) -> Result<ParsedAudioInputOptions> {
    let mut path = None;
    let mut audio_format = None;
    let mut refinement_level = DEFAULT_SPEECH_REFINEMENT_LEVEL.to_string();
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        if let Some(value) = arg.strip_prefix("--audio-format=") {
            audio_format = Some(required_inline_value("--audio-format", value)?.to_lowercase());
        } else if arg == "--audio-format" {
            index += 1;
            audio_format =
                Some(required_option_value(args, index, "--audio-format")?.to_lowercase());
        } else if let Some(value) = arg.strip_prefix("--refinement-level=") {
            refinement_level = required_inline_value("--refinement-level", value)?.to_string();
        } else if arg == "--refinement-level" {
            index += 1;
            refinement_level =
                required_option_value(args, index, "--refinement-level")?.to_string();
        } else if arg.starts_with("--") {
            anyhow::bail!("Unknown audio option: {arg}");
        } else if path.is_none() {
            path = Some(arg.clone());
        } else {
            anyhow::bail!("Usage: {usage}");
        }

        index += 1;
    }

    if refinement_level.trim().is_empty() {
        anyhow::bail!("--refinement-level requires a value");
    }
    if let Some(audio_format) = audio_format.as_deref()
        && audio_format.trim().is_empty()
    {
        anyhow::bail!("--audio-format requires a value");
    }

    Ok(ParsedAudioInputOptions {
        path: path.filter(|value| !value.trim().is_empty()),
        audio_format,
        refinement_level,
    })
}

fn parse_audio_input_options(args: &[String]) -> Result<(String, Option<String>, String)> {
    let usage = transcribe_usage();
    let parsed = parse_optional_audio_input_options(args, &usage)?;
    let path = parsed
        .path
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("Usage: {usage}"))?;
    Ok((path, parsed.audio_format, parsed.refinement_level))
}

fn interactive_audio_transcript(transcript: &str) -> String {
    let display_text = transcript.trim_matches('\n');
    let mut lines = display_text.lines();
    let Some(first) = lines.next() else {
        return "[transcript]".to_string();
    };
    let mut output = vec![format!("[transcript] {first}")];
    output.extend(lines.map(|line| format!("             {line}")));
    output.join("\n")
}

fn interactive_args_to_strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_string()).collect()
}

fn push_interactive_command_output(output: &mut Vec<String>, result: Result<String>) {
    match result {
        Ok(text) if text.is_empty() => {}
        Ok(text) => output.push(text),
        Err(error) => output.push(format!("Error: {error}")),
    }
}

fn output_format_description(raw: bool) -> &'static str {
    if raw { "Raw" } else { "Markdown" }
}

fn enabled_label(enabled: bool) -> &'static str {
    if enabled { "ENABLED" } else { "DISABLED" }
}

fn warning_lines(warnings: &[String]) -> Vec<String> {
    warnings
        .iter()
        .map(|warning| format!("Warning: {warning}"))
        .collect()
}

fn interactive_reasoning_warning() -> String {
    "/reason is deprecated and ignored since the Grok 4 release on 2025-07-09; reasoning is always enabled for all models.".to_string()
}

fn interactive_model_list(current_mode: &GrokMode) -> String {
    available_models_text(Some(current_mode), &GrokMode::known_modes())
}

async fn interactive_rate_limit_summary(mode: &GrokMode, debug: bool) -> Result<String> {
    let client = configured_client(debug)?;
    let Ok(rate_limit) = client.rate_limits_for_mode(mode).await else {
        return Ok(unavailable_rate_limit_summary(mode));
    };
    Ok(rate_limit_summary(&rate_limit, mode))
}

async fn handle_interactive_goal(
    args: &[&str],
    conversation_id: &mut Option<String>,
    parent_response_id: &mut Option<String>,
    parsed: &mut ParsedChatCommand,
    output: &mut Vec<String>,
) {
    let command = match parse_interactive_goal_command(args) {
        Ok(command) => command,
        Err(error) => {
            output.push(format!("Error: {error}"));
            return;
        }
    };

    match command {
        InteractiveGoalCommand::Show => output.push(goal_summary(parsed.goal.as_ref())),
        InteractiveGoalCommand::Create {
            objective,
            max_turns,
        } => {
            parsed.goal = Some(InteractiveGoalState::new(objective, max_turns));
            output.push("Goal started".to_string());
            run_goal_loop(conversation_id, parent_response_id, parsed, output).await;
        }
        InteractiveGoalCommand::Pause => {
            let Some(mut goal) = parsed.goal.clone() else {
                output.push("No active goal.".to_string());
                return;
            };
            goal.status = GoalStatus::Paused;
            parsed.goal = Some(goal);
            output.push("Goal paused".to_string());
        }
        InteractiveGoalCommand::Resume => {
            let Some(mut goal) = parsed.goal.clone() else {
                output.push("No active goal.".to_string());
                return;
            };
            if !matches!(goal.status, GoalStatus::Paused | GoalStatus::BudgetLimited) {
                output.push(goal_summary(Some(&goal)));
                return;
            }
            goal.status = GoalStatus::Active;
            if goal.turns_completed >= goal.max_turns {
                goal.max_turns = goal.turns_completed + InteractiveGoalState::DEFAULT_MAX_TURNS;
            }
            parsed.goal = Some(goal);
            output.push("Goal resumed".to_string());
            run_goal_loop(conversation_id, parent_response_id, parsed, output).await;
        }
        InteractiveGoalCommand::Clear => {
            parsed.goal = None;
            output.push("Goal cleared".to_string());
        }
        InteractiveGoalCommand::Complete => {
            let Some(mut goal) = parsed.goal.clone() else {
                output.push("No active goal.".to_string());
                return;
            };
            goal.status = GoalStatus::Complete;
            parsed.goal = Some(goal);
            output.push("Goal complete".to_string());
        }
    }
}

async fn run_goal_loop(
    conversation_id: &mut Option<String>,
    parent_response_id: &mut Option<String>,
    parsed: &mut ParsedChatCommand,
    output: &mut Vec<String>,
) {
    while should_continue_goal(parsed.goal.as_ref()) {
        let Some(mut goal) = parsed.goal.clone() else {
            return;
        };
        let prompt = if goal.turns_completed == 0 {
            initial_goal_prompt(&goal)
        } else {
            continuation_goal_prompt(&goal)
        };
        let file_attachments = if goal.turns_completed == 0 {
            parsed.file_attachment_ids.clone()
        } else {
            Vec::new()
        };

        let response = match send_goal_turn(
            &prompt,
            conversation_id.as_deref(),
            parent_response_id.as_deref(),
            parsed,
            file_attachments.clone(),
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                output.push(format!("Error: {error}"));
                return;
            }
        };

        if !file_attachments.is_empty() {
            parsed.file_attachment_ids.clear();
        }
        *conversation_id = Some(response.conversation_id.clone());
        *parent_response_id = Some(response.response_id.clone());
        output.push(response.message.clone());

        goal.turns_completed += 1;
        match goal_turn_result(&response.message) {
            GoalTurnResult::Complete => {
                goal.status = GoalStatus::Complete;
                parsed.goal = Some(goal);
                output.push("Goal complete".to_string());
                return;
            }
            GoalTurnResult::Paused => {
                goal.status = GoalStatus::Paused;
                parsed.goal = Some(goal);
                output.push("Goal paused".to_string());
                return;
            }
            GoalTurnResult::ContinueRunning => {}
        }
        if goal.turns_completed >= goal.max_turns {
            goal.status = GoalStatus::BudgetLimited;
            let max_turns = goal.max_turns;
            parsed.goal = Some(goal);
            output.push(format!("Goal stopped after {max_turns} turns."));
            return;
        }
        parsed.goal = Some(goal);
    }
}

async fn send_goal_turn(
    prompt: &str,
    conversation_id: Option<&str>,
    parent_response_id: Option<&str>,
    parsed: &ParsedChatCommand,
    file_attachments: Vec<String>,
) -> Result<grok_client::ConversationResponse> {
    let mut goal_parsed = parsed.clone();
    goal_parsed.file_attachment_ids = file_attachments;
    send_chat_turn(prompt, conversation_id, parent_response_id, &goal_parsed).await
}

fn continuation_parent_response_id(responses: &[GrokConversationMessage]) -> Option<String> {
    let parent_ids = responses
        .iter()
        .filter_map(|response| clean_response_token(response.parent_response_id.as_deref()))
        .collect::<HashSet<_>>();
    let indexed_responses = responses.iter().enumerate().collect::<Vec<_>>();

    latest_response_id(indexed_responses.iter().copied().filter(|(_, response)| {
        is_assistant_message(response) && !parent_ids.contains(response.response_id.trim())
    }))
    .or_else(|| {
        latest_response_id(
            indexed_responses
                .iter()
                .copied()
                .filter(|(_, response)| !parent_ids.contains(response.response_id.trim())),
        )
    })
    .or_else(|| {
        latest_response_id(
            indexed_responses
                .iter()
                .copied()
                .filter(|(_, response)| is_assistant_message(response)),
        )
    })
    .or_else(|| latest_response_id(indexed_responses.iter().copied()))
}

fn latest_response_id<'a>(
    candidates: impl Iterator<Item = (usize, &'a GrokConversationMessage)>,
) -> Option<String> {
    candidates
        .max_by(|lhs, rhs| compare_response_position(*lhs, *rhs))
        .map(|(_, response)| response.response_id.clone())
}

fn compare_response_position(
    lhs: (usize, &GrokConversationMessage),
    rhs: (usize, &GrokConversationMessage),
) -> Ordering {
    parsed_response_date(lhs.1)
        .cmp(&parsed_response_date(rhs.1))
        .then_with(|| lhs.0.cmp(&rhs.0))
}

fn parsed_response_date(response: &GrokConversationMessage) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(response.create_time.trim()).ok()
}

fn is_assistant_message(response: &GrokConversationMessage) -> bool {
    matches!(
        response.sender.trim().to_lowercase().as_str(),
        "assistant" | "grok" | "model"
    )
}

fn clean_response_token(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn most_recent_response_mode(responses: &[GrokConversationMessage]) -> Option<GrokMode> {
    for response in responses.iter().rev() {
        for value in [
            response.mode_id.as_deref(),
            response.model_id.as_deref(),
            response.mode_name.as_deref(),
            response.model_name.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(token) = clean_response_token(Some(value)) {
                return Some(GrokMode::resolve(Some(token)));
            }
        }
    }
    None
}

async fn share_current_conversation(
    conversation_id: Option<&str>,
    response_id: Option<&str>,
    debug: bool,
) -> Result<String> {
    let conversation_id = conversation_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("No current conversation to share"))?;
    let client = configured_client(debug)?;
    Ok(client
        .share_link_url(
            conversation_id,
            response_id,
            &GrokShareLinkOptions::default(),
        )
        .await?)
}

async fn delete_current_conversation(conversation_id: Option<&str>, debug: bool) -> Result<()> {
    let conversation_id = conversation_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("No current conversation to delete"))?;
    let client = configured_client(debug)?;
    Ok(client.soft_delete_conversation(conversation_id).await?)
}

async fn handle_interactive_skill_create(
    remainder: &str,
    conversation_id: &mut Option<String>,
    parent_response_id: &mut Option<String>,
    parsed: &mut ParsedChatCommand,
) -> Result<grok_client::ConversationResponse> {
    let prompt = parse_skill_create_prompt(remainder)?;
    let message = format!("skill-creator skill {prompt}");
    *conversation_id = None;
    *parent_response_id = None;
    parsed.selected_mode = GrokMode::grok43_beta();
    let response = send_chat_turn(&message, None, None, parsed).await?;
    *conversation_id = Some(response.conversation_id.clone());
    *parent_response_id = Some(response.response_id.clone());
    parsed.file_attachment_ids.clear();
    Ok(response)
}

fn copy_to_clipboard(text: &str) -> Result<()> {
    if let Ok(path) = std::env::var("GROK_CLIPBOARD_FILE")
        && !path.trim().is_empty()
    {
        std::fs::write(path, text)?;
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        use std::io::Write;
        use std::process::Stdio;

        let mut child = ProcessCommand::new("/usr/bin/pbcopy")
            .stdin(Stdio::piped())
            .spawn()?;
        if let Some(stdin) = child.stdin.as_mut() {
            stdin.write_all(text.as_bytes())?;
        }
        let status = child.wait()?;
        if status.success() {
            return Ok(());
        }
        anyhow::bail!("Could not copy to clipboard");
    }

    #[cfg(not(target_os = "macos"))]
    {
        anyhow::bail!("Clipboard copy is only supported on macOS");
    }
}

async fn send_chat_turn(
    message: &str,
    conversation_id: Option<&str>,
    parent_response_id: Option<&str>,
    parsed: &ParsedChatCommand,
) -> Result<grok_client::ConversationResponse> {
    let client = configured_client(parsed.debug)?;
    let options = chat_message_options(parsed);
    match conversation_id {
        Some(conversation_id) => Ok(client
            .continue_conversation_response(conversation_id, parent_response_id, message, &options)
            .await?),
        None => Ok(client.send_message_response(message, &options).await?),
    }
}

fn chat_message_options(parsed: &ParsedChatCommand) -> GrokMessageOptions {
    GrokMessageOptions {
        temporary: parsed.private_mode,
        mode_id: parsed.selected_mode.id.clone(),
        file_attachments: parsed.file_attachment_ids.clone(),
        workspace_ids: parsed.workspace_ids.clone(),
        ..GrokMessageOptions::default()
    }
}

fn parse_chat_command(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
) -> Result<ParsedChatCommand> {
    let mut message_words = Vec::new();
    let mut raw = top_level_options.raw;
    let mut quiet = false;
    let mut private_mode = top_level_options.private_mode;
    let mut stream = true;
    let mut debug = top_level_options.debug;
    let mut model = top_level_options.model.clone();
    let mut reasoning_requested = top_level_options.reasoning;
    let mut deep_search_requested = top_level_options.deep_search;
    let mut no_search_requested = top_level_options.no_search;
    let mut no_custom_instructions_requested = top_level_options.no_custom_instructions;
    let mut audio_path = None;
    let mut audio_format = None;
    let mut refinement_level = DEFAULT_SPEECH_REFINEMENT_LEVEL.to_string();
    let mut index = 0;

    if top_level_options.stream {
        stream = true;
    }
    if let Some(format) = top_level_options.format.as_deref() {
        let mut json_requested = false;
        apply_message_format(format, &mut json_requested, &mut raw)?;
        if json_requested {
            anyhow::bail!("JSON chat output is handled by the message command");
        }
    }

    while index < args.len() {
        let arg = &args[index];
        let next_value = args.get(index + 1).map(String::as_str);
        let model_option = apply_model_option(arg, next_value);
        if model_option.missing_value {
            anyhow::bail!("{arg} requires a model value");
        }
        if let Some(mode) = model_option.mode {
            model = Some(mode.id);
            if model_option.consumed_next {
                index += 1;
            }
            index += 1;
            continue;
        }

        let output_format_option = apply_output_format_option(arg, next_value);
        if output_format_option.missing_value {
            anyhow::bail!("{arg} requires a format value");
        }
        if let Some(invalid_value) = output_format_option.invalid_value {
            anyhow::bail!("Invalid output format '{invalid_value}'. Use md, raw, or json.");
        }
        if let Some(format) = output_format_option.format {
            if format.is_json() {
                anyhow::bail!("JSON chat output is handled by the message command");
            }
            match format {
                options::OutputFormat::Json => unreachable!("JSON chat format handled above"),
                options::OutputFormat::Raw => raw = true,
                options::OutputFormat::Markdown => raw = false,
            }
            if output_format_option.consumed_next {
                index += 1;
            }
            index += 1;
            continue;
        }

        match arg.as_str() {
            "--quiet" => quiet = true,
            "--private" => private_mode = true,
            "--stream" => stream = true,
            "--debug" => debug = true,
            "--reasoning" => reasoning_requested = true,
            "--deep-search" => deep_search_requested = true,
            "--no-search" => no_search_requested = true,
            "--no-custom-instructions" => no_custom_instructions_requested = true,
            "--stdin" => anyhow::bail!("--stdin is only supported by grok message"),
            "--prompt-file" => anyhow::bail!("--prompt-file is only supported by grok message"),
            "--audio" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.trim().is_empty() && !value.starts_with("--"))
                    .ok_or_else(|| anyhow::anyhow!("--audio requires a path or -"))?;
                audio_path = Some(value.clone());
                index += 1;
            }
            "--audio-format" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.trim().is_empty() && !value.starts_with("--"))
                    .ok_or_else(|| anyhow::anyhow!("--audio-format requires a value"))?;
                audio_format = Some(value.to_lowercase());
                index += 1;
            }
            "--refinement-level" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.trim().is_empty() && !value.starts_with("--"))
                    .ok_or_else(|| anyhow::anyhow!("--refinement-level requires a value"))?;
                refinement_level = value.clone();
                index += 1;
            }
            _ if arg.starts_with("--audio=") => {
                let value = arg.trim_start_matches("--audio=");
                if value.trim().is_empty() {
                    anyhow::bail!("--audio requires a path or -");
                }
                audio_path = Some(value.to_string());
            }
            _ if arg.starts_with("--audio-format=") => {
                let value = arg.trim_start_matches("--audio-format=");
                if value.trim().is_empty() {
                    anyhow::bail!("--audio-format requires a value");
                }
                audio_format = Some(value.to_lowercase());
            }
            _ if arg.starts_with("--refinement-level=") => {
                let value = arg.trim_start_matches("--refinement-level=");
                if value.trim().is_empty() {
                    anyhow::bail!("--refinement-level requires a value");
                }
                refinement_level = value.to_string();
            }
            _ => message_words.push(arg.clone()),
        }
        index += 1;
    }

    if audio_path.is_some() && !message_words.is_empty() {
        anyhow::bail!("Initial message arguments and --audio are mutually exclusive");
    }

    Ok(ParsedChatCommand {
        initial_message: (!message_words.is_empty()).then(|| message_words.join(" ")),
        initial_audio: audio_path.map(|path| ParsedAudioInputOptions {
            path: Some(path),
            audio_format,
            refinement_level,
        }),
        raw,
        quiet,
        private_mode,
        stream,
        typeahead_enabled: false,
        debug,
        selected_mode: GrokMode::resolve(model.as_deref()),
        file_attachment_ids: Vec::new(),
        workspace_ids: Vec::new(),
        workspace_label: None,
        goal: None,
        warnings: options::GrokCommandOptions::warnings_for(
            reasoning_requested,
            deep_search_requested,
            no_search_requested,
            no_custom_instructions_requested,
        ),
    })
}

async fn run_message_command(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
    command_name: &str,
) -> Result<String> {
    let json_requested_on_error = top_level_options.json || is_json_requested(&args);
    let mut parsed = match parse_message_command(args, top_level_options) {
        Ok(parsed) => parsed,
        Err(error) => {
            if json_requested_on_error {
                return json_error(command_name, None, error, "usage_error", 2);
            }
            return Ok(format!("Error: {error}"));
        }
    };

    let result = async {
        if let Some(audio_input) = parsed.audio_input.clone() {
            let (resolved, response) =
                transcribe_required_audio_input(&audio_input, parsed.debug, parsed.quiet).await?;
            parsed.resolved_audio_input = Some(message_audio_input_metadata(&resolved, &response));
            parsed.message = response.text;
            if parsed.message.trim().is_empty() {
                anyhow::bail!("Please provide a message to send");
            }
        }
        let client = configured_client(parsed.debug)?;
        let mut file_attachment_ids = parsed.file_attachment_ids.clone();
        for file_id in upload_message_files(&client, &parsed.file_upload_paths).await? {
            append_unique(file_id, &mut file_attachment_ids);
        }
        parsed.file_attachment_ids = file_attachment_ids.clone();
        let options = GrokMessageOptions {
            temporary: parsed.private_mode,
            mode_id: parsed.selected_mode.id.clone(),
            file_attachments: file_attachment_ids,
            ..GrokMessageOptions::default()
        };
        if parsed.stream && parsed.json {
            let request = client.new_conversation_request(&parsed.message, &options)?;
            let body = client.send_request_text(request).await?;
            return Ok::<_, anyhow::Error>(MessageCommandOutput::StreamingJson(
                streaming_message_json(&body, &parsed)?,
            ));
        }

        Ok::<_, anyhow::Error>(MessageCommandOutput::Response(
            client
                .send_message_response(&parsed.message, &options)
                .await?,
        ))
    }
    .await;

    match result {
        Ok(MessageCommandOutput::StreamingJson(output)) => Ok(output),
        Ok(MessageCommandOutput::Response(response)) if parsed.json => {
            let result = CliJsonResult::ok(
                command_name,
                None,
                "assistant_response",
                assistant_response_json(
                    &response,
                    &parsed.selected_mode,
                    message_request_json(&parsed),
                    message_audio_input_json(&parsed),
                ),
                default_json_meta(parsed.debug, parsed.warnings.clone()),
            );
            Ok(serde_json::to_string_pretty(&result)?)
        }
        Ok(MessageCommandOutput::Response(response)) => {
            let mut output = warning_lines(&parsed.warnings);
            output.push(message_output_text(&response.message, parsed.raw));
            Ok(output.join("\n"))
        }
        Err(error) if parsed.json => json_error(command_name, None, error, "api_error", 1),
        Err(error) => Ok(format!("Error: {error}")),
    }
}

fn parse_message_command(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
) -> Result<ParsedMessageCommand> {
    let mut message_words = Vec::new();
    let mut prompt_file = None;
    let mut explicit_stdin = false;
    let mut json_requested = top_level_options.json;
    let mut raw = top_level_options.raw;
    let mut quiet = false;
    let mut private_mode = top_level_options.private_mode;
    let mut stream = top_level_options.stream;
    let mut debug = top_level_options.debug;
    let mut model = top_level_options.model.clone();
    let mut reasoning_requested = top_level_options.reasoning;
    let mut deep_search_requested = top_level_options.deep_search;
    let mut no_search_requested = top_level_options.no_search;
    let mut no_custom_instructions_requested = top_level_options.no_custom_instructions;
    let mut file_attachment_ids = Vec::new();
    let mut file_upload_paths = Vec::new();
    let mut audio_path = None;
    let mut audio_format = None;
    let mut refinement_level = DEFAULT_SPEECH_REFINEMENT_LEVEL.to_string();
    let mut index = 0;

    if top_level_options
        .format
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("json"))
    {
        json_requested = true;
    }

    while index < args.len() {
        let arg = &args[index];
        let next_value = args.get(index + 1).map(String::as_str);
        let model_option = apply_model_option(arg, next_value);
        if model_option.missing_value {
            anyhow::bail!("{arg} requires a model value");
        }
        if let Some(mode) = model_option.mode {
            model = Some(mode.id);
            if model_option.consumed_next {
                index += 1;
            }
            index += 1;
            continue;
        }

        let output_format_option = apply_output_format_option(arg, next_value);
        if output_format_option.missing_value {
            anyhow::bail!("{arg} requires a format value");
        }
        if let Some(invalid_value) = output_format_option.invalid_value {
            anyhow::bail!("Invalid output format '{invalid_value}'. Use md, raw, or json.");
        }
        if let Some(format) = output_format_option.format {
            apply_message_output_format(format, &mut json_requested, &mut raw);
            if output_format_option.consumed_next {
                index += 1;
            }
            index += 1;
            continue;
        }

        match arg.as_str() {
            "--quiet" => quiet = true,
            "--private" => private_mode = true,
            "--stream" => stream = true,
            "--debug" => debug = true,
            "--reasoning" => reasoning_requested = true,
            "--deep-search" => deep_search_requested = true,
            "--no-search" => no_search_requested = true,
            "--no-custom-instructions" => no_custom_instructions_requested = true,
            "--stdin" => explicit_stdin = true,
            "--audio" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.trim().is_empty() && !value.starts_with("--"))
                    .ok_or_else(|| anyhow::anyhow!("--audio requires a path or -"))?;
                audio_path = Some(value.clone());
                index += 1;
            }
            "--audio-format" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.trim().is_empty() && !value.starts_with("--"))
                    .ok_or_else(|| anyhow::anyhow!("--audio-format requires a value"))?;
                audio_format = Some(value.to_lowercase());
                index += 1;
            }
            "--refinement-level" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.trim().is_empty() && !value.starts_with("--"))
                    .ok_or_else(|| anyhow::anyhow!("--refinement-level requires a value"))?;
                refinement_level = value.clone();
                index += 1;
            }
            "--prompt-file" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.trim().is_empty() && !value.starts_with("--"))
                    .ok_or_else(|| anyhow::anyhow!("--prompt-file requires a path"))?;
                prompt_file = Some(value.clone());
                index += 1;
            }
            "--attach" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.trim().is_empty() && !value.starts_with("--"))
                    .ok_or_else(|| anyhow::anyhow!("--attach requires a file ID"))?;
                append_unique(value.clone(), &mut file_attachment_ids);
                index += 1;
            }
            "--file" | "--upload" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.trim().is_empty() && !value.starts_with("--"))
                    .ok_or_else(|| anyhow::anyhow!("{arg} requires a path"))?;
                file_upload_paths.push(value.clone());
                index += 1;
            }
            _ if arg.starts_with("--audio=") => {
                let value = arg.trim_start_matches("--audio=");
                if value.trim().is_empty() {
                    anyhow::bail!("--audio requires a path or -");
                }
                audio_path = Some(value.to_string());
            }
            _ if arg.starts_with("--audio-format=") => {
                let value = arg.trim_start_matches("--audio-format=");
                if value.trim().is_empty() {
                    anyhow::bail!("--audio-format requires a value");
                }
                audio_format = Some(value.to_lowercase());
            }
            _ if arg.starts_with("--refinement-level=") => {
                let value = arg.trim_start_matches("--refinement-level=");
                if value.trim().is_empty() {
                    anyhow::bail!("--refinement-level requires a value");
                }
                refinement_level = value.to_string();
            }
            _ if arg.starts_with("--prompt-file=") => {
                let value = arg.trim_start_matches("--prompt-file=");
                if value.trim().is_empty() {
                    anyhow::bail!("--prompt-file requires a path");
                }
                prompt_file = Some(value.to_string());
            }
            _ if arg.starts_with("--attach=") => {
                let value = arg.trim_start_matches("--attach=");
                if value.trim().is_empty() {
                    anyhow::bail!("--attach requires a file ID");
                }
                append_unique(value.to_string(), &mut file_attachment_ids);
            }
            _ if arg.starts_with("--file=") => {
                let value = arg.trim_start_matches("--file=");
                if value.trim().is_empty() {
                    anyhow::bail!("--file requires a path");
                }
                file_upload_paths.push(value.to_string());
            }
            _ if arg.starts_with("--upload=") => {
                let value = arg.trim_start_matches("--upload=");
                if value.trim().is_empty() {
                    anyhow::bail!("--upload requires a path");
                }
                file_upload_paths.push(value.to_string());
            }
            _ => message_words.push(arg.clone()),
        }
        index += 1;
    }

    let prompt_source_count = usize::from(!message_words.is_empty())
        + usize::from(prompt_file.is_some())
        + usize::from(explicit_stdin)
        + usize::from(audio_path.is_some());
    if prompt_source_count > 1 {
        anyhow::bail!(
            "Inline message arguments, --audio, --prompt-file, and --stdin are mutually exclusive"
        );
    }

    let audio_input = audio_path.map(|path| ParsedAudioInputOptions {
        path: Some(path),
        audio_format,
        refinement_level,
    });

    let message = if audio_input.is_some() {
        String::new()
    } else if let Some(path) = prompt_file {
        std::fs::read_to_string(&path)
            .map_err(|error| anyhow::anyhow!("Could not read --prompt-file {path}: {error}"))?
    } else if explicit_stdin || message_words.is_empty() {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        input
    } else {
        message_words.join(" ")
    };

    if audio_input.is_none() && message.trim().is_empty() {
        let message = if explicit_stdin {
            "Please provide a message to send on stdin"
        } else {
            "Please provide a message to send"
        };
        anyhow::bail!("{message}");
    }

    Ok(ParsedMessageCommand {
        message,
        audio_input,
        resolved_audio_input: None,
        json: json_requested,
        raw,
        quiet,
        private_mode,
        stream,
        debug,
        selected_mode: GrokMode::resolve(model.as_deref()),
        file_attachment_ids,
        file_upload_paths,
        warnings: options::GrokCommandOptions::warnings_for(
            reasoning_requested,
            deep_search_requested,
            no_search_requested,
            no_custom_instructions_requested,
        ),
    })
}

async fn upload_message_files(client: &GrokClient, paths: &[String]) -> Result<Vec<String>> {
    let mut uploaded_file_ids = Vec::new();

    for path in paths {
        let upload_path = expand_tilde_path(PathBuf::from(path));
        let data = std::fs::read(&upload_path)
            .map_err(|error| anyhow::anyhow!("Could not read --file {path}: {error}"))?;
        let file_name = file_name_for_upload(&upload_path)?;
        let mime_type = inferred_file_mime_type(&upload_path);
        let content_base64 = general_purpose::STANDARD.encode(data);
        let response = client
            .upload_file_response(&file_name, &mime_type, &content_base64)
            .await?;
        let file_id = response
            .uploaded_file_id()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("Uploaded file response did not include an attachment ID")
            })?;
        append_unique(file_id.to_string(), &mut uploaded_file_ids);
    }

    Ok(uploaded_file_ids)
}

fn apply_message_format(value: &str, json_requested: &mut bool, raw: &mut bool) -> Result<()> {
    let Some(format) = options::OutputFormat::resolve(value) else {
        anyhow::bail!("Invalid output format '{value}'. Use md, raw, or json.");
    };
    apply_message_output_format(format, json_requested, raw);
    Ok(())
}

fn apply_message_output_format(
    format: options::OutputFormat,
    json_requested: &mut bool,
    raw: &mut bool,
) {
    match format {
        options::OutputFormat::Json => *json_requested = true,
        options::OutputFormat::Raw => *raw = true,
        options::OutputFormat::Markdown => *raw = false,
    }
}

fn message_output_text(message: &str, raw: bool) -> String {
    let visible = GrokStreamMarkupParser::visible_text(message, true);
    if raw {
        visible
    } else {
        render_terminal_markdown_subset(&visible)
    }
}

fn render_terminal_markdown_subset(markdown: &str) -> String {
    render_markdown_links(markdown)
        .lines()
        .map(render_markdown_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_markdown_line(line: &str) -> String {
    let trimmed_start = line.trim_start();
    let leading_whitespace = line.len() - trimmed_start.len();
    let without_heading = trimmed_start
        .strip_prefix("### ")
        .or_else(|| trimmed_start.strip_prefix("## "))
        .or_else(|| trimmed_start.strip_prefix("# "))
        .unwrap_or(trimmed_start);
    let prefix = &line[..leading_whitespace];
    format!("{prefix}{}", strip_inline_markdown_markers(without_heading))
}

fn strip_inline_markdown_markers(text: &str) -> String {
    text.replace("**", "").replace("__", "").replace('`', "")
}

fn render_markdown_links(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        let remainder = &text[index..];
        let Some(label_start_relative) = remainder.find('[') else {
            output.push_str(remainder);
            break;
        };
        let label_start = index + label_start_relative;
        output.push_str(&text[index..label_start]);

        let label_content_start = label_start + 1;
        let Some(label_end_relative) = text[label_content_start..].find(']') else {
            output.push_str(&text[label_start..]);
            break;
        };
        let label_end = label_content_start + label_end_relative;
        let open_paren = label_end + 1;
        if text[open_paren..].starts_with('(')
            && let Some(url_end_relative) = text[open_paren + 1..].find(')')
        {
            output.push_str(&text[label_content_start..label_end]);
            index = open_paren + 1 + url_end_relative + 1;
            continue;
        }

        output.push_str(&text[label_start..=label_end]);
        index = label_end + 1;
    }
    output
}

fn append_unique(value: String, values: &mut Vec<String>) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedCommandArgs {
    args: Vec<String>,
    json: bool,
    debug: bool,
}

impl ParsedCommandArgs {
    fn from_args(
        args: Vec<String>,
        top_level_options: &options::GrokCommandOptions,
    ) -> Result<Self> {
        let mut remaining = Vec::new();
        let mut json_requested = top_level_options.json;
        let mut debug_requested = top_level_options.debug;
        let mut index = 0;

        if let Some(format) = top_level_options.format.as_deref() {
            if format.trim().eq_ignore_ascii_case("json") {
                json_requested = true;
            } else {
                anyhow::bail!("Invalid --format value: {format}. Use json.");
            }
        }

        while index < args.len() {
            let arg = &args[index];
            if arg == "--json" {
                json_requested = true;
                index += 1;
                continue;
            }
            if let Some(value) = arg.strip_prefix("--format=") {
                if value.is_empty() {
                    anyhow::bail!("--format requires a value");
                }
                if !value.trim().eq_ignore_ascii_case("json") {
                    anyhow::bail!("Invalid --format value: {value}. Use json.");
                }
                json_requested = true;
                index += 1;
                continue;
            }
            if arg == "--format" {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.starts_with("--"))
                    .ok_or_else(|| anyhow::anyhow!("--format requires a value"))?;
                if !value.trim().eq_ignore_ascii_case("json") {
                    anyhow::bail!("Invalid --format value: {value}. Use json.");
                }
                json_requested = true;
                index += 2;
                continue;
            }
            if arg == "--debug" {
                debug_requested = true;
                index += 1;
                continue;
            }

            remaining.push(arg.clone());
            index += 1;
        }

        Ok(Self {
            args: remaining,
            json: json_requested,
            debug: debug_requested,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ListAction {
    List,
    Delete { conversation_id: String },
    ConversationHistory { conversation_id: String },
    Help(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedListCommand {
    action: ListAction,
    json: bool,
    debug: bool,
}

async fn run_list_command(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
) -> Result<String> {
    let json_requested_on_error = top_level_options.json || is_json_requested(&args);
    let parsed = match parse_list_command(args, top_level_options) {
        Ok(parsed) => parsed,
        Err(error) => {
            if json_requested_on_error {
                return json_error("list", None, error, "usage_error", 2);
            }
            return Ok(format!("Error: {error}"));
        }
    };

    match parsed.action.clone() {
        ListAction::List => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                Ok::<_, anyhow::Error>(
                    client
                        .list_conversations_response(&GrokConversationListOptions::default())
                        .await?,
                )
            }
            .await;

            match result {
                Ok(response) if parsed.json => {
                    let result = CliJsonResult::ok(
                        "list",
                        None,
                        "conversation_list",
                        conversations_json(&response.conversations),
                        default_json_meta(parsed.debug, top_level_options.warnings()),
                    );
                    Ok(serde_json::to_string_pretty(&result)?)
                }
                Ok(response) => Ok(conversation_rows(&response.conversations)),
                Err(error) if parsed.json => json_error("list", None, error, "api_error", 1),
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        ListAction::Delete { conversation_id } => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                Ok::<_, anyhow::Error>(client.soft_delete_conversation(&conversation_id).await?)
            }
            .await;

            match result {
                Ok(()) if parsed.json => {
                    let result = CliJsonResult::ok(
                        "list",
                        Some("delete".to_string()),
                        "conversation_delete",
                        json!({
                            "action": "soft_delete",
                            "conversationId": conversation_id,
                            "deleted": true
                        }),
                        default_json_meta(parsed.debug, top_level_options.warnings()),
                    );
                    Ok(serde_json::to_string_pretty(&result)?)
                }
                Ok(()) => Ok(format!("Deleted conversation {conversation_id}.")),
                Err(error) if parsed.json => {
                    json_error("list", Some("delete".to_string()), error, "api_error", 1)
                }
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        ListAction::ConversationHistory { conversation_id } => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                Ok::<_, anyhow::Error>(client.load_responses(&conversation_id, None).await?)
            }
            .await;

            match result {
                Ok(responses) if parsed.json => {
                    let result = CliJsonResult::ok(
                        "list",
                        Some("conversation".to_string()),
                        "conversation_history",
                        conversation_history_json(&conversation_id, &responses),
                        default_json_meta(parsed.debug, top_level_options.warnings()),
                    );
                    Ok(serde_json::to_string_pretty(&result)?)
                }
                Ok(responses) => Ok(conversation_history_rows(&responses)),
                Err(error) if parsed.json => json_error(
                    "list",
                    Some("conversation".to_string()),
                    error,
                    "api_error",
                    1,
                ),
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        ListAction::Help(usage) => Ok(usage),
    }
}

fn parse_list_command(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
) -> Result<ParsedListCommand> {
    let parsed_args = ParsedCommandArgs::from_args(args, top_level_options)?;
    let action = match parsed_args.args.first().map(String::as_str) {
        Some("delete" | "remove") => ListAction::Delete {
            conversation_id: parse_list_delete_conversation_id(&parsed_args.args[1..])?,
        },
        Some(value) if router::is_help_argument(value) || value == "help" => {
            ListAction::Help(list_usage())
        }
        _ => parse_list_query_action(&parsed_args.args)?,
    };

    Ok(ParsedListCommand {
        action,
        json: parsed_args.json,
        debug: parsed_args.debug,
    })
}

fn parse_list_query_action(args: &[String]) -> Result<ListAction> {
    let mut conversation_id = None;
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        if router::is_help_argument(arg) {
            return Ok(ListAction::Help(list_usage()));
        }
        if arg == "--conversation" {
            let value = args
                .get(index + 1)
                .filter(|value| !value.starts_with("--"))
                .ok_or_else(|| anyhow::anyhow!("--conversation requires a conversation ID"))?;
            conversation_id = Some(value.clone());
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--conversation=") {
            if value.is_empty() {
                anyhow::bail!("--conversation requires a conversation ID");
            }
            conversation_id = Some(value.to_string());
            index += 1;
            continue;
        }

        anyhow::bail!("Unknown option for list: {arg}");
    }

    Ok(match conversation_id {
        Some(conversation_id) => ListAction::ConversationHistory { conversation_id },
        None => ListAction::List,
    })
}

fn parse_list_delete_conversation_id(args: &[String]) -> Result<String> {
    let mut confirmed = false;
    let mut conversation_id = None;
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        if router::is_help_argument(arg) {
            anyhow::bail!("{}", list_delete_usage());
        }
        if arg == "--yes" || arg == "-y" {
            confirmed = true;
        } else if arg == "--conversation" {
            let value = args
                .get(index + 1)
                .filter(|value| !value.starts_with("--"))
                .ok_or_else(|| anyhow::anyhow!("--conversation requires a conversation ID"))?;
            conversation_id = Some(value.clone());
            index += 1;
        } else if let Some(value) = arg.strip_prefix("--conversation=") {
            if value.is_empty() {
                anyhow::bail!("--conversation requires a conversation ID");
            }
            conversation_id = Some(value.to_string());
        } else if arg.starts_with('-') {
            anyhow::bail!("Unknown option for list delete: {arg}");
        } else if conversation_id.is_none() {
            conversation_id = Some(arg.clone());
        } else {
            anyhow::bail!("list delete accepts one conversation ID");
        }

        index += 1;
    }

    let conversation_id = conversation_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("list delete requires a conversation ID"))?;
    if !confirmed {
        anyhow::bail!("list delete requires --yes");
    }

    Ok(conversation_id)
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum FilesAction {
    Upload {
        path: String,
        mime_type: Option<String>,
    },
    List {
        page_size: usize,
    },
    Delete {
        file_id: String,
    },
    Help(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedFilesCommand {
    action: FilesAction,
    json: bool,
    debug: bool,
}

async fn run_files_command(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
) -> Result<String> {
    let json_requested_on_error = top_level_options.json || is_json_requested(&args);
    let parsed = match parse_files_command(args, top_level_options) {
        Ok(parsed) => parsed,
        Err(error) => {
            if json_requested_on_error {
                return json_error("files", None, error, "usage_error", 2);
            }
            return Ok(format!("Error: {error}"));
        }
    };

    match parsed.action.clone() {
        FilesAction::Upload { path, mime_type } => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                let upload_path = expand_tilde_path(PathBuf::from(&path));
                let data = std::fs::read(&upload_path)?;
                let file_name = file_name_for_upload(&upload_path)?;
                let resolved_mime_type =
                    mime_type.unwrap_or_else(|| inferred_file_mime_type(&upload_path));
                let content_base64 = general_purpose::STANDARD.encode(data);
                Ok::<_, anyhow::Error>(
                    client
                        .upload_file_response(&file_name, &resolved_mime_type, &content_base64)
                        .await?,
                )
            }
            .await;

            match result {
                Ok(response) if parsed.json => {
                    let uploaded_id = response.uploaded_file_id().map(ToOwned::to_owned);
                    let result = CliJsonResult::ok(
                        "files",
                        Some("upload".to_string()),
                        "resource_mutation",
                        resource_mutation_json(
                            "file",
                            "upload",
                            uploaded_id.as_deref(),
                            Some(upload_response_json(&response)),
                            [],
                        ),
                        default_json_meta(parsed.debug, top_level_options.warnings()),
                    );
                    Ok(serde_json::to_string_pretty(&result)?)
                }
                Ok(response) => Ok(format!(
                    "Uploaded file\nID: {}\nFile: {}",
                    response.uploaded_file_id().unwrap_or(""),
                    response.file_name.as_deref().unwrap_or(&path)
                )),
                Err(error) if parsed.json => {
                    json_error("files", Some("upload".to_string()), error, "api_error", 1)
                }
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        FilesAction::List { page_size } => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                let options = GrokAssetListOptions {
                    page_size,
                    ..GrokAssetListOptions::default()
                };
                Ok::<_, anyhow::Error>(client.list_assets_response(&options).await?)
            }
            .await;

            match result {
                Ok(response) if parsed.json => {
                    let items = response.assets.iter().map(asset_json).collect::<Vec<_>>();
                    let result = CliJsonResult::ok(
                        "files",
                        Some("list".to_string()),
                        "resource_list",
                        resource_list_json(
                            "file",
                            items,
                            [("pageSize".to_string(), json!(page_size))],
                        ),
                        default_json_meta(parsed.debug, top_level_options.warnings()),
                    );
                    Ok(serde_json::to_string_pretty(&result)?)
                }
                Ok(response) => Ok(asset_rows(&response.assets)),
                Err(error) if parsed.json => {
                    json_error("files", Some("list".to_string()), error, "api_error", 1)
                }
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        FilesAction::Delete { file_id } => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                Ok::<_, anyhow::Error>(client.delete_asset(&file_id).await?)
            }
            .await;

            match result {
                Ok(response) if parsed.json => {
                    let item = response.asset.as_ref().map(asset_json);
                    let result = CliJsonResult::ok(
                        "files",
                        Some("delete".to_string()),
                        "resource_mutation",
                        resource_mutation_json("file", "delete", Some(&file_id), item, []),
                        default_json_meta(parsed.debug, top_level_options.warnings()),
                    );
                    Ok(serde_json::to_string_pretty(&result)?)
                }
                Ok(_) => Ok(format!("Deleted file {file_id}")),
                Err(error) if parsed.json => {
                    json_error("files", Some("delete".to_string()), error, "api_error", 1)
                }
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        FilesAction::Help(usage) => Ok(usage),
    }
}

fn parse_files_command(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
) -> Result<ParsedFilesCommand> {
    let parsed_args = ParsedCommandArgs::from_args(args, top_level_options)?;
    let action = match parsed_args.args.first().map(String::as_str) {
        None => FilesAction::List { page_size: 9 },
        Some(value) if router::is_help_argument(value) || value == "help" => {
            FilesAction::Help(files_usage())
        }
        Some("list") => {
            let args = &parsed_args.args[1..];
            if args.iter().any(|arg| router::is_help_argument(arg)) {
                FilesAction::Help(format!("Usage: {}", files_list_usage()))
            } else {
                FilesAction::List {
                    page_size: parse_files_list_page_size(args)?,
                }
            }
        }
        Some("upload") => {
            let args = &parsed_args.args[1..];
            if args.iter().any(|arg| router::is_help_argument(arg)) {
                FilesAction::Help(format!("Usage: {}", files_upload_usage()))
            } else {
                let (path, mime_type) = parse_files_upload_options(args)?;
                FilesAction::Upload { path, mime_type }
            }
        }
        Some("delete" | "remove") => {
            let args = &parsed_args.args[1..];
            if args.iter().any(|arg| router::is_help_argument(arg)) {
                FilesAction::Help(format!("Usage: {}", files_delete_usage()))
            } else {
                FilesAction::Delete {
                    file_id: parse_files_delete_id(args)?,
                }
            }
        }
        Some(command) => anyhow::bail!("Unknown files command: {command}\n{}", files_usage()),
    };

    Ok(ParsedFilesCommand {
        action,
        json: parsed_args.json,
        debug: parsed_args.debug,
    })
}

fn parse_files_upload_options(args: &[String]) -> Result<(String, Option<String>)> {
    let mut path = None;
    let mut mime_type = None;
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        if let Some(value) = arg.strip_prefix("--mime=") {
            if value.trim().is_empty() {
                anyhow::bail!("--mime requires a value");
            }
            mime_type = Some(value.to_string());
        } else if arg == "--mime" {
            let value = args
                .get(index + 1)
                .filter(|value| !value.trim().is_empty() && !value.starts_with("--"))
                .ok_or_else(|| anyhow::anyhow!("--mime requires a value"))?;
            mime_type = Some(value.clone());
            index += 1;
        } else if arg.starts_with("--") {
            anyhow::bail!(
                "Unknown option for files upload: {arg}\n{}",
                files_upload_usage()
            );
        } else if path.is_none() {
            path = Some(arg.clone());
        } else {
            anyhow::bail!("Usage: {}", files_upload_usage());
        }

        index += 1;
    }

    let path =
        path.ok_or_else(|| anyhow::anyhow!("Missing file path.\n{}", files_upload_usage()))?;
    Ok((path, mime_type))
}

fn parse_files_list_page_size(args: &[String]) -> Result<usize> {
    let mut page_size = 9;
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        if let Some(value) = arg.strip_prefix("--page-size=") {
            page_size = parse_positive_usize(value, "--page-size")?;
        } else if arg == "--page-size" {
            let value = args
                .get(index + 1)
                .ok_or_else(|| anyhow::anyhow!("Missing value for --page-size"))?;
            page_size = parse_positive_usize(value, "--page-size")?;
            index += 1;
        } else {
            anyhow::bail!(
                "Unknown option for files list: {arg}\n{}",
                files_list_usage()
            );
        }

        index += 1;
    }

    Ok(page_size)
}

fn parse_files_delete_id(args: &[String]) -> Result<String> {
    if args.len() != 1 || args[0].trim().is_empty() {
        anyhow::bail!("Usage: {}", files_delete_usage());
    }
    Ok(args[0].clone())
}

fn parse_positive_usize(value: &str, option: &str) -> Result<usize> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| anyhow::anyhow!("{option} must be a positive integer"))?;
    if parsed == 0 {
        anyhow::bail!("{option} must be a positive integer");
    }
    Ok(parsed)
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WorkspacesAction {
    List,
    Create(GrokWorkspaceCreateOptions),
    AddConversation {
        workspace_id: String,
        conversation_id: String,
    },
    Delete {
        workspace_id: String,
    },
    Conversation {
        conversation_id: String,
    },
    Help(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedWorkspacesCommand {
    action: WorkspacesAction,
    json: bool,
    debug: bool,
}

async fn run_workspaces_command(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
) -> Result<String> {
    let json_requested_on_error = top_level_options.json || is_json_requested(&args);
    let parsed = match parse_workspaces_command(args, top_level_options) {
        Ok(parsed) => parsed,
        Err(error) => {
            if json_requested_on_error {
                return json_error("workspaces", None, error, "usage_error", 2);
            }
            return Ok(format!("Error: {error}"));
        }
    };

    match parsed.action.clone() {
        WorkspacesAction::List => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                Ok::<_, anyhow::Error>(
                    client
                        .list_workspaces_response(&GrokWorkspaceListOptions::default())
                        .await?,
                )
            }
            .await;

            match result {
                Ok(response) if parsed.json => {
                    let items = response
                        .workspaces
                        .iter()
                        .map(workspace_json)
                        .collect::<Vec<_>>();
                    let result = CliJsonResult::ok(
                        "workspaces",
                        Some("list".to_string()),
                        "resource_list",
                        resource_list_json("workspace", items, []),
                        default_json_meta(parsed.debug, top_level_options.warnings()),
                    );
                    Ok(serde_json::to_string_pretty(&result)?)
                }
                Ok(response) => Ok(workspace_rows(&response.workspaces)),
                Err(error) if parsed.json => json_error(
                    "workspaces",
                    Some("list".to_string()),
                    error,
                    "api_error",
                    1,
                ),
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        WorkspacesAction::Create(options) => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                Ok::<_, anyhow::Error>(client.create_workspace_response(&options).await?)
            }
            .await;

            match result {
                Ok(response) if parsed.json => {
                    let item = response.workspace.as_ref().map(workspace_json);
                    let id = response
                        .workspace
                        .as_ref()
                        .and_then(GrokWorkspace::resolved_id);
                    let result = CliJsonResult::ok(
                        "workspaces",
                        Some("create".to_string()),
                        "resource_mutation",
                        resource_mutation_json("workspace", "create", id, item, []),
                        default_json_meta(parsed.debug, top_level_options.warnings()),
                    );
                    Ok(serde_json::to_string_pretty(&result)?)
                }
                Ok(response) => Ok(match response.workspace.as_ref() {
                    Some(workspace) => {
                        format!("Created workspace\n{}", workspace_summary(workspace))
                    }
                    None => "Created workspace".to_string(),
                }),
                Err(error) if parsed.json => json_error(
                    "workspaces",
                    Some("create".to_string()),
                    error,
                    "api_error",
                    1,
                ),
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        WorkspacesAction::AddConversation {
            workspace_id,
            conversation_id,
        } => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                Ok::<_, anyhow::Error>(
                    client
                        .add_conversation_to_workspace(&workspace_id, &conversation_id)
                        .await?,
                )
            }
            .await;

            match result {
                Ok(response) if parsed.json => {
                    let item = response.workspace.as_ref().map(workspace_json);
                    let result = CliJsonResult::ok(
                        "workspaces",
                        Some("add-conversation".to_string()),
                        "resource_mutation",
                        resource_mutation_json(
                            "workspace",
                            "addConversation",
                            Some(&workspace_id),
                            item,
                            [("conversationId".to_string(), json!(conversation_id))],
                        ),
                        default_json_meta(parsed.debug, top_level_options.warnings()),
                    );
                    Ok(serde_json::to_string_pretty(&result)?)
                }
                Ok(_) => Ok(format!(
                    "Added conversation {conversation_id} to workspace {workspace_id}"
                )),
                Err(error) if parsed.json => json_error(
                    "workspaces",
                    Some("add-conversation".to_string()),
                    error,
                    "api_error",
                    1,
                ),
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        WorkspacesAction::Delete { workspace_id } => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                Ok::<_, anyhow::Error>(client.delete_workspace(&workspace_id).await?)
            }
            .await;

            match result {
                Ok(response) if parsed.json => {
                    let item = response.workspace.as_ref().map(workspace_json);
                    let result = CliJsonResult::ok(
                        "workspaces",
                        Some("delete".to_string()),
                        "resource_mutation",
                        resource_mutation_json(
                            "workspace",
                            "delete",
                            Some(&workspace_id),
                            item,
                            [],
                        ),
                        default_json_meta(parsed.debug, top_level_options.warnings()),
                    );
                    Ok(serde_json::to_string_pretty(&result)?)
                }
                Ok(_) => Ok(format!("Deleted workspace {workspace_id}")),
                Err(error) if parsed.json => json_error(
                    "workspaces",
                    Some("delete".to_string()),
                    error,
                    "api_error",
                    1,
                ),
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        WorkspacesAction::Conversation { conversation_id } => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                Ok::<_, anyhow::Error>(
                    client
                        .get_conversation_v2(&conversation_id, true, true)
                        .await?,
                )
            }
            .await;

            match result {
                Ok(response) if parsed.json => {
                    let result = CliJsonResult::ok(
                        "workspaces",
                        Some("conversation".to_string()),
                        "conversation_detail",
                        conversation_v2_json(&response, &conversation_id),
                        default_json_meta(parsed.debug, top_level_options.warnings()),
                    );
                    Ok(serde_json::to_string_pretty(&result)?)
                }
                Ok(response) => Ok(conversation_v2_rows(&response, &conversation_id)),
                Err(error) if parsed.json => json_error(
                    "workspaces",
                    Some("conversation".to_string()),
                    error,
                    "api_error",
                    1,
                ),
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        WorkspacesAction::Help(usage) => Ok(usage),
    }
}

fn parse_workspaces_command(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
) -> Result<ParsedWorkspacesCommand> {
    let parsed_args = ParsedCommandArgs::from_args(args, top_level_options)?;
    let action = match parsed_args.args.first().map(String::as_str) {
        None => WorkspacesAction::List,
        Some(value) if router::is_help_argument(value) || value == "help" => {
            WorkspacesAction::Help(workspaces_usage())
        }
        Some("list") => {
            if parsed_args
                .args
                .iter()
                .skip(1)
                .any(|arg| router::is_help_argument(arg))
            {
                return Ok(ParsedWorkspacesCommand {
                    action: WorkspacesAction::Help(workspaces_list_usage()),
                    json: parsed_args.json,
                    debug: parsed_args.debug,
                });
            }
            if parsed_args.args.len() != 1 {
                anyhow::bail!("{}", workspaces_list_usage());
            }
            WorkspacesAction::List
        }
        Some("create") => {
            if parsed_args
                .args
                .iter()
                .skip(1)
                .any(|arg| router::is_help_argument(arg))
            {
                return Ok(ParsedWorkspacesCommand {
                    action: WorkspacesAction::Help(workspaces_create_usage()),
                    json: parsed_args.json,
                    debug: parsed_args.debug,
                });
            }
            WorkspacesAction::Create(parse_workspace_create_options(&parsed_args.args[1..])?)
        }
        Some("add-conversation") => {
            if parsed_args
                .args
                .iter()
                .skip(1)
                .any(|arg| router::is_help_argument(arg))
            {
                return Ok(ParsedWorkspacesCommand {
                    action: WorkspacesAction::Help(workspaces_add_conversation_usage()),
                    json: parsed_args.json,
                    debug: parsed_args.debug,
                });
            }
            if parsed_args.args.len() != 3 {
                anyhow::bail!("{}", workspaces_add_conversation_usage());
            }
            WorkspacesAction::AddConversation {
                workspace_id: parsed_args.args[1].clone(),
                conversation_id: parsed_args.args[2].clone(),
            }
        }
        Some("delete" | "remove") => {
            if parsed_args
                .args
                .iter()
                .skip(1)
                .any(|arg| router::is_help_argument(arg))
            {
                return Ok(ParsedWorkspacesCommand {
                    action: WorkspacesAction::Help(workspaces_delete_usage()),
                    json: parsed_args.json,
                    debug: parsed_args.debug,
                });
            }
            if parsed_args.args.len() != 2 {
                anyhow::bail!("{}", workspaces_delete_usage());
            }
            WorkspacesAction::Delete {
                workspace_id: parsed_args.args[1].clone(),
            }
        }
        Some("conversation") => {
            if parsed_args
                .args
                .iter()
                .skip(1)
                .any(|arg| router::is_help_argument(arg))
            {
                return Ok(ParsedWorkspacesCommand {
                    action: WorkspacesAction::Help(workspaces_conversation_usage()),
                    json: parsed_args.json,
                    debug: parsed_args.debug,
                });
            }
            if parsed_args.args.len() != 2 {
                anyhow::bail!("{}", workspaces_conversation_usage());
            }
            WorkspacesAction::Conversation {
                conversation_id: parsed_args.args[1].clone(),
            }
        }
        Some(command) => {
            anyhow::bail!(
                "Unknown workspaces command: {command}\n{}",
                workspaces_usage()
            );
        }
    };

    Ok(ParsedWorkspacesCommand {
        action,
        json: parsed_args.json,
        debug: parsed_args.debug,
    })
}

fn parse_workspace_create_options(args: &[String]) -> Result<GrokWorkspaceCreateOptions> {
    let mut options = GrokWorkspaceCreateOptions::default();
    let mut name = None;
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        if let Some(value) = arg.strip_prefix("--name=") {
            name = Some(required_inline_value("--name", value)?.to_string());
        } else if arg == "--name" {
            index += 1;
            name = Some(required_option_value(args, index, "--name")?.to_string());
        } else if let Some(value) = arg.strip_prefix("--icon=") {
            options.icon = required_inline_value("--icon", value)?.to_string();
        } else if arg == "--icon" {
            index += 1;
            options.icon = required_option_value(args, index, "--icon")?.to_string();
        } else if let Some(value) = arg.strip_prefix("--personality=") {
            options.custom_personality = required_inline_value("--personality", value)?.to_string();
        } else if arg == "--personality" {
            index += 1;
            options.custom_personality =
                required_option_value(args, index, "--personality")?.to_string();
        } else if let Some(value) = arg.strip_prefix("--model=") {
            options.preferred_model = required_inline_value("--model", value)?.to_string();
        } else if arg == "--model" {
            index += 1;
            options.preferred_model = required_option_value(args, index, "--model")?.to_string();
        } else {
            anyhow::bail!(
                "Unknown option for workspaces create: {arg}\n{}",
                workspaces_create_usage()
            );
        }

        index += 1;
    }

    let name = name
        .filter(|value: &String| !value.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Missing required option: --name <name>\n{}",
                workspaces_create_usage()
            )
        })?;
    options.name = name;
    Ok(options)
}

fn required_option_value<'a>(args: &'a [String], index: usize, option: &str) -> Result<&'a str> {
    args.get(index)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("{option} requires a value"))
}

fn required_inline_value<'a>(option: &str, value: &'a str) -> Result<&'a str> {
    (!value.trim().is_empty())
        .then_some(value)
        .ok_or_else(|| anyhow::anyhow!("{option} requires a value"))
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SkillsAction {
    List,
    User,
    Help(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedSkillsCommand {
    action: SkillsAction,
    json: bool,
    debug: bool,
}

async fn run_skills_command(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
) -> Result<String> {
    let json_requested_on_error = top_level_options.json || is_json_requested(&args);
    let parsed = match parse_skills_command(args, top_level_options) {
        Ok(parsed) => parsed,
        Err(error) => {
            if json_requested_on_error {
                return json_error("skills", None, error, "usage_error", 2);
            }
            return Ok(format!("Error: {error}"));
        }
    };

    match parsed.action {
        SkillsAction::List => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                let built_in = client.list_skills_response("en").await?;
                let user = client.list_user_skills_response().await?;
                Ok::<_, anyhow::Error>((built_in, user))
            }
            .await;

            match result {
                Ok((built_in, user)) if parsed.json => {
                    let built_in_items = built_in.skills.iter().map(skill_json).collect::<Vec<_>>();
                    let user_items = user.skills.iter().map(skill_json).collect::<Vec<_>>();
                    let result = CliJsonResult::ok(
                        "skills",
                        Some("list".to_string()),
                        "resource_list",
                        resource_list_json(
                            "skill",
                            built_in_items.clone(),
                            [
                                ("scope".to_string(), json!("available")),
                                ("builtInSkills".to_string(), Value::Array(built_in_items)),
                                ("userSkills".to_string(), Value::Array(user_items)),
                            ],
                        ),
                        default_json_meta(parsed.debug, top_level_options.warnings()),
                    );
                    Ok(serde_json::to_string_pretty(&result)?)
                }
                Ok((built_in, user)) => Ok(skill_sections(&built_in.skills, &user.skills)),
                Err(error) if parsed.json => {
                    json_error("skills", Some("list".to_string()), error, "api_error", 1)
                }
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        SkillsAction::User => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                Ok::<_, anyhow::Error>(client.list_user_skills_response().await?)
            }
            .await;

            match result {
                Ok(response) if parsed.json => {
                    let items = response.skills.iter().map(skill_json).collect::<Vec<_>>();
                    let result = CliJsonResult::ok(
                        "skills",
                        Some("user".to_string()),
                        "resource_list",
                        resource_list_json("skill", items, [("scope".to_string(), json!("user"))]),
                        default_json_meta(parsed.debug, top_level_options.warnings()),
                    );
                    Ok(serde_json::to_string_pretty(&result)?)
                }
                Ok(response) => Ok(skill_rows("User Skills", &response.skills)),
                Err(error) if parsed.json => {
                    json_error("skills", Some("user".to_string()), error, "api_error", 1)
                }
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        SkillsAction::Help(usage) => Ok(usage),
    }
}

fn parse_skills_command(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
) -> Result<ParsedSkillsCommand> {
    let parsed_args = ParsedCommandArgs::from_args(args, top_level_options)?;
    let action = match parsed_args.args.first().map(String::as_str) {
        None => SkillsAction::List,
        Some(value) if router::is_help_argument(value) || value == "help" => {
            SkillsAction::Help(skills_usage())
        }
        Some("list") => {
            if parsed_args
                .args
                .iter()
                .skip(1)
                .any(|arg| router::is_help_argument(arg))
            {
                SkillsAction::Help(skills_list_usage())
            } else if parsed_args.args.len() == 1 {
                SkillsAction::List
            } else {
                anyhow::bail!("{}", skills_list_usage());
            }
        }
        Some("mine" | "user") => {
            if parsed_args
                .args
                .iter()
                .skip(1)
                .any(|arg| router::is_help_argument(arg))
            {
                if parsed_args
                    .args
                    .first()
                    .is_some_and(|command| command == "mine")
                {
                    SkillsAction::Help(skills_mine_usage())
                } else {
                    SkillsAction::Help(skills_user_usage())
                }
            } else if parsed_args.args.len() == 1 {
                SkillsAction::User
            } else {
                let command = parsed_args
                    .args
                    .first()
                    .map(String::as_str)
                    .unwrap_or("user");
                anyhow::bail!("Usage: grok skills {command} [--json|--format json] [--debug]");
            }
        }
        Some(command) => {
            anyhow::bail!("Unknown skills command: {command}\n{}", skills_usage());
        }
    };

    Ok(ParsedSkillsCommand {
        action,
        json: parsed_args.json,
        debug: parsed_args.debug,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TasksAction {
    List,
    Inactive,
    Select,
    Show(String),
    Results(TaskResultsOptions),
    Chat(TaskChatCommandOptions),
    Create(TaskCreateCommandOptions),
    Archive(String),
    Help(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedTasksCommand {
    action: TasksAction,
    json: bool,
    debug: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TaskResultsOptions {
    task_id: String,
    limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TaskChatCommandOptions {
    task_id: String,
    run_selector: TaskRunSelector,
    limit: usize,
    message: Option<String>,
    mode: GrokMode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TaskRunSelector {
    Latest,
    Previous,
    ResultId(String),
    Ordinal(usize),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TaskCreateCommandOptions {
    prompt: String,
    name: Option<String>,
    date: String,
    time: String,
    timezone: String,
    guideline: Option<String>,
    model_mode: String,
}

async fn run_tasks_command(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
) -> Result<String> {
    let json_requested_on_error = top_level_options.json || is_json_requested(&args);
    let parsed = match parse_tasks_command(args, top_level_options) {
        Ok(parsed) => parsed,
        Err(error) => {
            if json_requested_on_error {
                return json_error("tasks", None, error, "usage_error", 2);
            }
            return Ok(format!("Error: {error}"));
        }
    };

    match parsed.action.clone() {
        TasksAction::List => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                Ok::<_, anyhow::Error>(client.list_tasks_response().await?)
            }
            .await;

            match result {
                Ok(response) if parsed.json => {
                    let tasks = display_ordered_tasks(response.tasks);
                    let result = CliJsonResult::ok(
                        "tasks",
                        Some("list".to_string()),
                        "resource_list",
                        resource_list_json_vec(
                            "task",
                            tasks.iter().map(task_json).collect(),
                            Vec::new(),
                        ),
                        default_json_meta(parsed.debug, top_level_options.warnings()),
                    );
                    Ok(serde_json::to_string_pretty(&result)?)
                }
                Ok(response) => Ok(task_rows(&display_ordered_tasks(response.tasks))),
                Err(error) if parsed.json => {
                    json_error("tasks", Some("list".to_string()), error, "api_error", 1)
                }
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        TasksAction::Inactive => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                Ok::<_, anyhow::Error>(client.list_inactive_tasks_response().await?)
            }
            .await;

            match result {
                Ok(response) if parsed.json => {
                    let result = CliJsonResult::ok(
                        "tasks",
                        Some("inactive".to_string()),
                        "resource_list",
                        resource_list_json_vec(
                            "task",
                            response.tasks.iter().map(task_json).collect(),
                            Vec::new(),
                        ),
                        default_json_meta(parsed.debug, top_level_options.warnings()),
                    );
                    Ok(serde_json::to_string_pretty(&result)?)
                }
                Ok(response) => Ok(task_rows(&response.tasks)),
                Err(error) if parsed.json => {
                    json_error("tasks", Some("inactive".to_string()), error, "api_error", 1)
                }
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        TasksAction::Select => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                Ok::<_, anyhow::Error>(client.list_tasks_response().await?)
            }
            .await;

            match result {
                Ok(response) if parsed.json => {
                    let tasks = display_ordered_tasks(response.tasks);
                    let result = CliJsonResult::ok(
                        "tasks",
                        Some("select".to_string()),
                        "resource_list",
                        resource_list_json_vec(
                            "task",
                            tasks.iter().map(task_json).collect(),
                            Vec::new(),
                        ),
                        default_json_meta(parsed.debug, top_level_options.warnings()),
                    );
                    Ok(serde_json::to_string_pretty(&result)?)
                }
                Ok(response) => Ok(task_rows(&display_ordered_tasks(response.tasks))),
                Err(error) if parsed.json => {
                    json_error("tasks", Some("select".to_string()), error, "api_error", 1)
                }
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        TasksAction::Show(task_id) => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                let response = client.list_tasks_response().await?;
                let task = task_matching(&task_id, &response.tasks)?;
                let latest_result = if let Some(resolved_id) = task_identifier(&task) {
                    client.latest_task_result(&resolved_id).await?
                } else {
                    None
                };
                Ok::<_, anyhow::Error>((task, latest_result))
            }
            .await;

            match result {
                Ok((task, latest_result)) if parsed.json => {
                    let result = CliJsonResult::ok(
                        "tasks",
                        Some("show".to_string()),
                        "resource_detail",
                        json!({
                            "resource": "task",
                            "item": task_detail_json(&task, latest_result.as_ref())
                        }),
                        default_json_meta(parsed.debug, top_level_options.warnings()),
                    );
                    Ok(serde_json::to_string_pretty(&result)?)
                }
                Ok((task, latest_result)) => Ok(task_detail(&task, latest_result.as_ref())),
                Err(error) if parsed.json => {
                    json_error("tasks", Some("show".to_string()), error, "api_error", 1)
                }
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        TasksAction::Results(options) => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                let (task_id, task) = resolve_task_reference(&client, &options.task_id).await?;
                let results = client
                    .task_results_response(&task_id, options.limit)
                    .await?
                    .results;
                Ok::<_, anyhow::Error>((task_id, task, results))
            }
            .await;

            match result {
                Ok((task_id, task, results)) if parsed.json => {
                    let mut data = Map::new();
                    data.insert("resource".to_string(), json!("task_result"));
                    data.insert("taskId".to_string(), json!(task_id));
                    data.insert("limit".to_string(), json!(options.limit));
                    if let Some(task) = task.as_ref() {
                        data.insert("task".to_string(), task_json(task));
                    }
                    if let Some(result) = results.first() {
                        data.insert("latestResult".to_string(), task_result_json(result));
                    }
                    if options.limit > 1 {
                        data.insert(
                            "results".to_string(),
                            Value::Array(results.iter().map(task_result_json).collect()),
                        );
                    }
                    let result = CliJsonResult::ok(
                        "tasks",
                        Some("results".to_string()),
                        "resource_detail",
                        Value::Object(data),
                        default_json_meta(parsed.debug, top_level_options.warnings()),
                    );
                    Ok(serde_json::to_string_pretty(&result)?)
                }
                Ok((task_id, _task, results)) => Ok(task_result_rows(&task_id, &results)),
                Err(error) if parsed.json => {
                    json_error("tasks", Some("results".to_string()), error, "api_error", 1)
                }
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        TasksAction::Chat(options) => run_task_chat(options, parsed, top_level_options).await,
        TasksAction::Create(options) => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                let create_options = GrokTaskCreateOptions {
                    name: options.name.clone().unwrap_or_default(),
                    metadata_json_string: "{}".to_string(),
                    schedule: Some(GrokTaskSchedule::once(
                        options.date.clone(),
                        options.time.clone(),
                        options.timezone.clone(),
                    )),
                    notification_method: "DEFAULT".to_string(),
                    model_mode: options.model_mode.clone(),
                    notification_decider_enable: true,
                    notification_decider_guideline: options
                        .guideline
                        .clone()
                        .unwrap_or_else(|| "only notify if it's economically valuable".to_string()),
                    model_name: String::new(),
                    toolset: vec![String::new()],
                };
                Ok::<_, anyhow::Error>(client.create_task(&options.prompt, &create_options).await?)
            }
            .await;

            match result {
                Ok(response) if parsed.json => task_mutation_output(
                    "create",
                    "create",
                    response,
                    parsed.debug,
                    top_level_options,
                ),
                Ok(response) => {
                    let mut lines = vec!["Created task".to_string()];
                    if let Some(task) = response.task.as_ref() {
                        lines.push(task_summary(task));
                    }
                    Ok(lines.join("\n"))
                }
                Err(error) if parsed.json => {
                    json_error("tasks", Some("create".to_string()), error, "api_error", 1)
                }
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        TasksAction::Archive(task_id) => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                Ok::<_, anyhow::Error>(client.archive_task(&task_id, false).await?)
            }
            .await;

            match result {
                Ok(response) if parsed.json => task_mutation_output(
                    "archive",
                    "archive",
                    response,
                    parsed.debug,
                    top_level_options,
                ),
                Ok(_response) => Ok(format!("Archived task {task_id}")),
                Err(error) if parsed.json => {
                    json_error("tasks", Some("archive".to_string()), error, "api_error", 1)
                }
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        TasksAction::Help(usage) => Ok(usage),
    }
}

fn task_mutation_output(
    subcommand: &str,
    action: &str,
    response: GrokTaskMutationResponse,
    debug: bool,
    top_level_options: &options::GrokCommandOptions,
) -> Result<String> {
    let item = response.task.as_ref().map(task_json);
    let id = response
        .task
        .as_ref()
        .and_then(task_identifier)
        .map(|value| value.to_string());
    let result = CliJsonResult::ok(
        "tasks",
        Some(subcommand.to_string()),
        "resource_mutation",
        resource_mutation_json_vec("task", action, id.as_deref(), item, Vec::new()),
        default_json_meta(debug, top_level_options.warnings()),
    );
    Ok(serde_json::to_string_pretty(&result)?)
}

async fn run_task_chat(
    options: TaskChatCommandOptions,
    parsed: ParsedTasksCommand,
    top_level_options: &options::GrokCommandOptions,
) -> Result<String> {
    let result = async {
        let client = configured_client(parsed.debug)?;
        let (task_id, task) = resolve_task_reference(&client, &options.task_id).await?;
        let results = client
            .task_results_response(&task_id, options.limit)
            .await?
            .results;
        let (index, run) = select_task_run(results, &options.run_selector)?;
        let conversation_id = task_result_conversation_id(&run)
            .ok_or_else(|| anyhow::anyhow!("Task run is missing conversationId"))?;
        let result_response_id = task_result_response_id(&run);
        let parent_response_id = selected_run_parent_response_id(&run)
            .or_else(|| result_response_id.clone())
            .ok_or_else(|| anyhow::anyhow!("Task run is missing responseId"))?;

        let _ = client
            .get_conversation_v2(&conversation_id, true, true)
            .await
            .ok();
        if let Some(result_response_id) = result_response_id.as_ref() {
            let response_ids = vec![result_response_id.clone()];
            let _ = client
                .load_responses(&conversation_id, Some(&response_ids))
                .await
                .ok();
        }

        let response = if let Some(message) = options.message.as_deref() {
            let message_options = GrokMessageOptions {
                mode_id: options.mode.id.clone(),
                ..GrokMessageOptions::default()
            };
            Some(
                client
                    .continue_conversation_response(
                        &conversation_id,
                        Some(&parent_response_id),
                        message,
                        &message_options,
                    )
                    .await?,
            )
        } else {
            None
        };

        Ok::<_, anyhow::Error>((
            task_id,
            task,
            index,
            run,
            conversation_id,
            parent_response_id,
            result_response_id,
            response,
        ))
    }
    .await;

    match result {
        Ok((
            task_id,
            task,
            index,
            run,
            conversation_id,
            parent_response_id,
            result_response_id,
            response,
        )) if parsed.json => {
            let mut data = Map::new();
            data.insert("taskId".to_string(), json!(task_id));
            data.insert(
                "run".to_string(),
                task_run_json(&task_id, task.as_ref(), &run, index),
            );
            data.insert("conversationId".to_string(), json!(conversation_id));
            data.insert("parentResponseId".to_string(), json!(parent_response_id));
            if let Some(result_response_id) = result_response_id {
                data.insert("resultResponseId".to_string(), json!(result_response_id));
            }
            if let Some(response) = response {
                data.insert(
                    "response".to_string(),
                    assistant_response_json(
                        &response,
                        &options.mode,
                        json!({
                            "reasoning": true,
                            "deepSearch": false,
                            "noSearch": false,
                            "private": false,
                            "stream": false,
                            "workspaceIds": [],
                            "fileAttachmentIds": []
                        }),
                        None,
                    ),
                );
            }
            let result = CliJsonResult::ok(
                "tasks",
                Some("chat".to_string()),
                "assistant_response",
                Value::Object(data),
                default_json_meta(parsed.debug, top_level_options.warnings()),
            );
            Ok(serde_json::to_string_pretty(&result)?)
        }
        Ok((
            _task_id,
            task,
            index,
            run,
            conversation_id,
            parent_response_id,
            result_response_id,
            response,
        )) => {
            let mut lines = vec!["Opened task run chat".to_string()];
            if let Some(title) = task.as_ref().and_then(task_title) {
                lines.push(format!("task: {title}"));
            }
            lines.push(format!("run: {}", run_ordinal_label(index)));
            if let Some(result_id) = run.resolved_id() {
                lines.push(format!("resultId: {result_id}"));
            }
            lines.push(format!("conversationId: {conversation_id}"));
            if let Some(result_response_id) = result_response_id {
                lines.push(format!("responseId: {result_response_id}"));
            }
            lines.push(format!("parentResponseId: {parent_response_id}"));
            if let Some(response) = response {
                lines.push(String::new());
                lines.push(response.message);
            }
            Ok(lines.join("\n"))
        }
        Err(error) if parsed.json => {
            json_error("tasks", Some("chat".to_string()), error, "api_error", 1)
        }
        Err(error) => Ok(format!("Error: {error}")),
    }
}

fn parse_tasks_command(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
) -> Result<ParsedTasksCommand> {
    let (remaining, json, debug) = parse_task_global_options(args, top_level_options)?;
    let action = match remaining.first().map(String::as_str) {
        None => TasksAction::List,
        Some(value) if router::is_help_argument(value) || value == "help" => {
            TasksAction::Help(tasks_usage())
        }
        Some("list") => {
            if remaining
                .iter()
                .skip(1)
                .any(|arg| router::is_help_argument(arg))
            {
                TasksAction::Help(tasks_list_usage())
            } else {
                let list_args = &remaining[1..];
                if list_args.is_empty() {
                    TasksAction::List
                } else if list_args.len() == 1
                    && (list_args[0] == "--inactive" || list_args[0] == "--archived")
                {
                    TasksAction::Inactive
                } else {
                    anyhow::bail!("{}", tasks_list_usage());
                }
            }
        }
        Some("inactive" | "archived") => {
            if remaining
                .iter()
                .skip(1)
                .any(|arg| router::is_help_argument(arg))
            {
                TasksAction::Help(tasks_inactive_usage())
            } else if remaining.len() == 1 {
                TasksAction::Inactive
            } else {
                anyhow::bail!("{}", tasks_inactive_usage());
            }
        }
        Some("select") => {
            if remaining
                .iter()
                .skip(1)
                .any(|arg| router::is_help_argument(arg))
            {
                TasksAction::Help(tasks_select_usage())
            } else if remaining.len() == 1 {
                TasksAction::Select
            } else {
                anyhow::bail!("{}", tasks_select_usage());
            }
        }
        Some("show" | "details" | "detail") => {
            if remaining
                .iter()
                .skip(1)
                .any(|arg| router::is_help_argument(arg))
            {
                TasksAction::Help(tasks_show_usage())
            } else if remaining.len() == 2 {
                TasksAction::Show(remaining[1].clone())
            } else {
                anyhow::bail!("{}", tasks_show_usage());
            }
        }
        Some("results" | "result") => {
            if remaining
                .iter()
                .skip(1)
                .any(|arg| router::is_help_argument(arg))
            {
                TasksAction::Help(tasks_results_usage())
            } else {
                TasksAction::Results(parse_task_results_options(&remaining[1..])?)
            }
        }
        Some("chat" | "thread" | "open") => {
            if remaining
                .iter()
                .skip(1)
                .any(|arg| router::is_help_argument(arg))
            {
                TasksAction::Help(tasks_chat_usage())
            } else {
                TasksAction::Chat(parse_task_chat_options(&remaining[1..])?)
            }
        }
        Some("create") => {
            if remaining
                .iter()
                .skip(1)
                .any(|arg| router::is_help_argument(arg))
            {
                TasksAction::Help(tasks_create_usage())
            } else {
                TasksAction::Create(parse_task_create_options(&remaining[1..])?)
            }
        }
        Some("archive") => {
            if remaining
                .iter()
                .skip(1)
                .any(|arg| router::is_help_argument(arg))
            {
                TasksAction::Help(tasks_archive_usage())
            } else if remaining.len() == 2 {
                TasksAction::Archive(remaining[1].clone())
            } else {
                anyhow::bail!("{}", tasks_archive_usage());
            }
        }
        Some(command) => anyhow::bail!("Unknown tasks command: {command}\n{}", tasks_usage()),
    };
    Ok(ParsedTasksCommand {
        action,
        json,
        debug,
    })
}

fn parse_task_global_options(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
) -> Result<(Vec<String>, bool, bool)> {
    let mut remaining = Vec::new();
    let mut json = top_level_options.json;
    let mut debug = top_level_options.debug;
    let mut index = 0;

    if let Some(format) = top_level_options.format.as_deref() {
        if format.trim().eq_ignore_ascii_case("json") {
            json = true;
        } else {
            anyhow::bail!("Invalid --format value: {format}. Use json.");
        }
    }

    while index < args.len() {
        let arg = &args[index];
        if arg == "--json" {
            json = true;
        } else if arg == "--format" {
            index += 1;
            let value = args
                .get(index)
                .filter(|value| !value.starts_with("--"))
                .ok_or_else(|| anyhow::anyhow!("--format requires a value"))?;
            if value.trim().eq_ignore_ascii_case("json") {
                json = true;
            } else {
                anyhow::bail!("Invalid --format value: {value}. Use json.");
            }
        } else if let Some(value) = arg.strip_prefix("--format=") {
            if value.is_empty() {
                anyhow::bail!("--format requires a value");
            }
            if value.trim().eq_ignore_ascii_case("json") {
                json = true;
            } else {
                anyhow::bail!("Invalid --format value: {value}. Use json.");
            }
        } else if arg == "--debug" {
            debug = true;
        } else {
            remaining.push(arg.clone());
        }
        index += 1;
    }
    Ok((remaining, json, debug))
}

fn parse_task_results_options(args: &[String]) -> Result<TaskResultsOptions> {
    let (remaining, limit) = remove_limit_option(args, 1)?;
    if remaining.len() != 1 {
        anyhow::bail!("{}", tasks_results_usage());
    }
    Ok(TaskResultsOptions {
        task_id: remaining[0].clone(),
        limit,
    })
}

fn parse_task_chat_options(args: &[String]) -> Result<TaskChatCommandOptions> {
    let mut task_id = None;
    let mut run_selector = TaskRunSelector::Latest;
    let mut limit = 10;
    let mut message = None;
    let mut message_words = Vec::new();
    let mut model = None;
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        if let Some(value) = arg.strip_prefix("--run=") {
            run_selector = parse_task_run_selector(required_inline_value("--run", value)?);
        } else if arg == "--run" {
            index += 1;
            run_selector = parse_task_run_selector(required_option_value(args, index, "--run")?);
        } else if let Some(value) = arg.strip_prefix("--result-id=") {
            run_selector =
                TaskRunSelector::ResultId(required_inline_value("--result-id", value)?.to_string());
        } else if arg == "--result-id" {
            index += 1;
            run_selector = TaskRunSelector::ResultId(
                required_option_value(args, index, "--result-id")?.to_string(),
            );
        } else if let Some(value) = arg.strip_prefix("--run-index=") {
            run_selector = parse_task_run_index(required_inline_value("--run-index", value)?)?;
        } else if arg == "--run-index" {
            index += 1;
            run_selector =
                parse_task_run_index(required_option_value(args, index, "--run-index")?)?;
        } else if let Some(value) = arg.strip_prefix("--limit=") {
            limit = parse_positive_limit(required_inline_value("--limit", value)?)?;
        } else if arg == "--limit" {
            index += 1;
            limit = parse_positive_limit(required_option_value(args, index, "--limit")?)?;
        } else if let Some(value) = arg.strip_prefix("--message=") {
            message = Some(required_inline_value("--message", value)?.to_string());
        } else if arg == "--message" {
            index += 1;
            message = Some(required_option_value(args, index, "--message")?.to_string());
        } else if let Some(value) = arg.strip_prefix("--model=") {
            model = Some(required_inline_value("--model", value)?.to_string());
        } else if let Some(value) = arg.strip_prefix("--mode=") {
            model = Some(required_inline_value("--mode", value)?.to_string());
        } else if arg == "--model" || arg == "--mode" {
            index += 1;
            model = Some(required_option_value(args, index, arg)?.to_string());
        } else if arg.starts_with("--") {
            anyhow::bail!(
                "Unknown option for tasks chat: {arg}\n{}",
                tasks_chat_usage()
            );
        } else if task_id.is_none() {
            task_id = Some(arg.clone());
        } else {
            message_words.push(arg.clone());
        }
        index += 1;
    }

    if message.is_none() && !message_words.is_empty() {
        message = Some(message_words.join(" "));
    }
    if matches!(run_selector, TaskRunSelector::Previous) {
        limit = limit.max(2);
    }
    let task_id = task_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("{}", tasks_chat_usage()))?;

    Ok(TaskChatCommandOptions {
        task_id,
        run_selector,
        limit,
        message,
        mode: GrokMode::resolve(model.as_deref()),
    })
}

fn parse_task_run_selector(value: &str) -> TaskRunSelector {
    let trimmed = value.trim();
    match trimmed.to_lowercase().as_str() {
        "" | "latest" | "newest" => TaskRunSelector::Latest,
        "previous" | "prev" => TaskRunSelector::Previous,
        _ => trimmed
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0)
            .map(|value| TaskRunSelector::Ordinal(value - 1))
            .unwrap_or_else(|| TaskRunSelector::ResultId(trimmed.to_string())),
    }
}

fn parse_task_run_index(value: &str) -> Result<TaskRunSelector> {
    let index = value
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| anyhow::anyhow!("--run-index must be a positive integer"))?;
    Ok(TaskRunSelector::Ordinal(index - 1))
}

fn parse_task_create_options(args: &[String]) -> Result<TaskCreateCommandOptions> {
    let mut prompt = None;
    let mut name = None;
    let mut date = None;
    let mut time = None;
    let mut timezone = None;
    let mut guideline = None;
    let mut model_mode = "BASE".to_string();
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        if let Some(value) = arg.strip_prefix("--prompt=") {
            prompt = Some(required_inline_value("--prompt", value)?.to_string());
        } else if arg == "--prompt" {
            index += 1;
            prompt = Some(required_option_value(args, index, "--prompt")?.to_string());
        } else if let Some(value) = arg.strip_prefix("--name=") {
            name = Some(required_inline_value("--name", value)?.to_string());
        } else if arg == "--name" {
            index += 1;
            name = Some(required_option_value(args, index, "--name")?.to_string());
        } else if let Some(value) = arg.strip_prefix("--date=") {
            date = Some(required_inline_value("--date", value)?.to_string());
        } else if arg == "--date" {
            index += 1;
            date = Some(required_option_value(args, index, "--date")?.to_string());
        } else if let Some(value) = arg.strip_prefix("--time=") {
            time = Some(required_inline_value("--time", value)?.to_string());
        } else if arg == "--time" {
            index += 1;
            time = Some(required_option_value(args, index, "--time")?.to_string());
        } else if let Some(value) = arg.strip_prefix("--timezone=") {
            timezone = Some(required_inline_value("--timezone", value)?.to_string());
        } else if arg == "--timezone" {
            index += 1;
            timezone = Some(required_option_value(args, index, "--timezone")?.to_string());
        } else if let Some(value) = arg.strip_prefix("--guideline=") {
            guideline = Some(required_inline_value("--guideline", value)?.to_string());
        } else if arg == "--guideline" {
            index += 1;
            guideline = Some(required_option_value(args, index, "--guideline")?.to_string());
        } else if let Some(value) = arg.strip_prefix("--model-mode=") {
            model_mode = required_inline_value("--model-mode", value)?.to_string();
        } else if arg == "--model-mode" {
            index += 1;
            model_mode = required_option_value(args, index, "--model-mode")?.to_string();
        } else {
            anyhow::bail!(
                "Unknown option for tasks create: {arg}\n{}",
                tasks_create_usage()
            );
        }
        index += 1;
    }

    let prompt = prompt
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Missing required option: --prompt <text>\n{}",
                tasks_create_usage()
            )
        })?;
    let timezone = timezone.unwrap_or_else(|| "Asia/Bangkok".to_string());
    let (default_date, default_time) = current_task_datetime_strings(&timezone);
    Ok(TaskCreateCommandOptions {
        prompt,
        name,
        date: date.unwrap_or(default_date),
        time: time.unwrap_or(default_time),
        timezone,
        guideline,
        model_mode,
    })
}

fn remove_limit_option(args: &[String], default_value: usize) -> Result<(Vec<String>, usize)> {
    let mut remaining = Vec::new();
    let mut limit = default_value;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if let Some(value) = arg.strip_prefix("--limit=") {
            limit = parse_positive_limit(required_inline_value("--limit", value)?)?;
        } else if arg == "--limit" {
            index += 1;
            limit = parse_positive_limit(required_option_value(args, index, "--limit")?)?;
        } else {
            remaining.push(arg.clone());
        }
        index += 1;
    }
    Ok((remaining, limit))
}

fn parse_positive_limit(value: &str) -> Result<usize> {
    value
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| anyhow::anyhow!("--limit must be a positive integer"))
}

fn current_task_datetime_strings(timezone: &str) -> (String, String) {
    task_datetime_strings_at(Utc::now(), timezone)
}

fn task_datetime_strings_at(now: DateTime<Utc>, timezone: &str) -> (String, String) {
    let timezone = timezone.parse::<Tz>().unwrap_or(chrono_tz::Asia::Bangkok);
    let local_time = now.with_timezone(&timezone);
    (
        local_time.format("%Y-%m-%d").to_string(),
        local_time.format("%H:%M").to_string(),
    )
}

async fn resolve_task_reference(
    client: &GrokClient,
    reference: &str,
) -> Result<(String, Option<GrokTask>)> {
    let trimmed = reference.trim();
    let response = client.list_tasks_response().await?;
    if let Ok(task) = task_matching(trimmed, &response.tasks)
        && let Some(task_id) = task_identifier(&task)
    {
        return Ok((task_id, Some(task)));
    }
    Ok((trimmed.to_string(), None))
}

fn select_task_run(
    results: Vec<GrokTaskResult>,
    selector: &TaskRunSelector,
) -> Result<(usize, GrokTaskResult)> {
    if results.is_empty() {
        anyhow::bail!("No task runs found");
    }
    match selector {
        TaskRunSelector::Latest => Ok((0, results[0].clone())),
        TaskRunSelector::Previous => results
            .get(1)
            .cloned()
            .map(|result| (1, result))
            .ok_or_else(|| anyhow::anyhow!("No previous task run found")),
        TaskRunSelector::Ordinal(index) => results
            .get(*index)
            .cloned()
            .map(|result| (*index, result))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Task run index {} is outside the {} loaded runs",
                    index + 1,
                    results.len()
                )
            }),
        TaskRunSelector::ResultId(id) => {
            let needle = id.trim();
            results
                .iter()
                .enumerate()
                .find(|(_, result)| {
                    result.resolved_id() == Some(needle)
                        || task_result_response_id(result).as_deref() == Some(needle)
                        || string_in_value(&result.raw_json, &["id"]).as_deref() == Some(needle)
                })
                .map(|(index, result)| (index, result.clone()))
                .ok_or_else(|| anyhow::anyhow!("Task run not found: {id}"))
        }
    }
}

fn task_run_json(
    task_id: &str,
    task: Option<&GrokTask>,
    result: &GrokTaskResult,
    index: usize,
) -> Value {
    let mut data = Map::new();
    data.insert("taskId".to_string(), json!(task_id));
    data.insert("index".to_string(), json!(index));
    data.insert("ordinal".to_string(), json!(run_ordinal_label(index)));
    data.insert("result".to_string(), task_result_json(result));
    if let Some(task_title) = task.and_then(task_title) {
        data.insert("taskTitle".to_string(), json!(task_title));
    }
    if let Some(timestamp) = task_result_timestamp(result) {
        data.insert(
            "timestampLabel".to_string(),
            json!(task_run_timestamp_label(&timestamp)),
        );
        data.insert("timestamp".to_string(), json!(timestamp));
    }
    if let Some(conversation_id) = task_result_conversation_id(result) {
        data.insert("conversationId".to_string(), json!(conversation_id));
    }
    if let Some(response_id) = task_result_response_id(result) {
        data.insert("responseId".to_string(), json!(response_id));
    }
    Value::Object(data)
}

fn task_result_conversation_id(result: &GrokTaskResult) -> Option<String> {
    result
        .conversation_id
        .clone()
        .or_else(|| string_in_value(&result.raw_json, &["conversationId", "conversation_id"]))
}

fn task_result_response_id(result: &GrokTaskResult) -> Option<String> {
    result
        .response_id
        .clone()
        .or_else(|| string_in_value(&result.raw_json, &["responseId", "response_id"]))
}

fn selected_run_parent_response_id(result: &GrokTaskResult) -> Option<String> {
    string_in_value(
        &result.raw_json,
        &[
            "parentResponseId",
            "parent_response_id",
            "threadParentResponseId",
            "thread_parent_response_id",
        ],
    )
}

fn task_matching(reference: &str, tasks: &[GrokTask]) -> Result<GrokTask> {
    let normalized = reference.trim().to_lowercase();
    tasks
        .iter()
        .find(|task| {
            task_identifier(task).as_deref() == Some(reference)
                || task_title(task)
                    .as_deref()
                    .is_some_and(|title| title.trim().to_lowercase() == normalized)
        })
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Task not found: {reference}"))
}

fn display_ordered_tasks(mut tasks: Vec<GrokTask>) -> Vec<GrokTask> {
    tasks.sort_by_key(|task| task_status(task).as_deref() == Some("paused"));
    tasks
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum AgentsAction {
    List,
    Show(i64),
    Set(AgentSetOptions),
    Edit { agent_id: i64, replace: bool },
    Clear(AgentSetOptions),
    Help(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AgentSetOptions {
    agent_id: i64,
    name: Option<String>,
    instructions: String,
    replace: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedAgentsCommand {
    action: AgentsAction,
    json: bool,
    debug: bool,
    include_instructions: bool,
}

async fn run_agents_command(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
) -> Result<String> {
    let json_requested_on_error = top_level_options.json || is_json_requested(&args);
    let parsed = match parse_agents_command(args, top_level_options) {
        Ok(parsed) => parsed,
        Err(error) => {
            if json_requested_on_error {
                return json_error("agents", None, error, "usage_error", 2);
            }
            return Ok(format!("Error: {error}"));
        }
    };

    match parsed.action.clone() {
        AgentsAction::List => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                Ok::<_, anyhow::Error>(client.get_user_settings_response().await?)
            }
            .await;

            match result {
                Ok(response) if parsed.json => {
                    let agents = if response.agent_customizations.is_empty() {
                        default_agent_customizations()
                    } else {
                        response.agent_customizations
                    };
                    let items = agents
                        .iter()
                        .map(|agent| agent_json(agent, parsed.include_instructions))
                        .collect::<Vec<_>>();
                    let extra = if parsed.include_instructions {
                        Vec::new()
                    } else {
                        vec![("rawRedacted".to_string(), json!(true))]
                    };
                    let result = CliJsonResult::ok(
                        "agents",
                        Some("list".to_string()),
                        "resource_list",
                        resource_list_json_vec("agent", items, extra),
                        default_json_meta(parsed.debug, top_level_options.warnings()),
                    );
                    Ok(serde_json::to_string_pretty(&result)?)
                }
                Ok(response) => {
                    let agents = if response.agent_customizations.is_empty() {
                        default_agent_customizations()
                    } else {
                        response.agent_customizations
                    };
                    Ok(agent_rows(&agents))
                }
                Err(error) if parsed.json => {
                    json_error("agents", Some("list".to_string()), error, "api_error", 1)
                }
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        AgentsAction::Show(agent_id) => {
            let result = async {
                let client = configured_client(parsed.debug)?;
                let response = client.get_user_settings_response().await?;
                let agents = if response.agent_customizations.is_empty() {
                    default_agent_customizations()
                } else {
                    response.agent_customizations
                };
                agent_customization(agent_id, &agents)
            }
            .await;

            match result {
                Ok(agent) if parsed.json => {
                    let result = CliJsonResult::ok(
                        "agents",
                        Some("show".to_string()),
                        "resource_detail",
                        json!({
                            "resource": "agent",
                            "item": agent_json(&agent, true)
                        }),
                        default_json_meta(parsed.debug, top_level_options.warnings()),
                    );
                    Ok(serde_json::to_string_pretty(&result)?)
                }
                Ok(agent) => Ok(agent_detail(&agent)),
                Err(error) if parsed.json => {
                    json_error("agents", Some("show".to_string()), error, "api_error", 1)
                }
                Err(error) => Ok(format!("Error: {error}")),
            }
        }
        AgentsAction::Set(options) => {
            run_agent_mutation("set", "set", options, parsed, top_level_options).await
        }
        AgentsAction::Clear(options) => {
            run_agent_mutation("clear", "clear", options, parsed, top_level_options).await
        }
        AgentsAction::Edit { agent_id, replace } => {
            run_agent_edit(agent_id, replace, parsed, top_level_options).await
        }
        AgentsAction::Help(usage) => Ok(usage),
    }
}

async fn run_agent_mutation(
    subcommand: &str,
    action: &str,
    options: AgentSetOptions,
    parsed: ParsedAgentsCommand,
    top_level_options: &options::GrokCommandOptions,
) -> Result<String> {
    let result = async {
        let client = configured_client(parsed.debug)?;
        let current = current_agent_customizations(&client, options.replace).await?;
        let updated = merge_agent_customization(&options, &current)?;
        client.update_agent_customizations(&updated).await?;
        Ok::<_, anyhow::Error>(updated)
    }
    .await;

    match result {
        Ok(updated) if parsed.json => {
            let item = updated
                .iter()
                .find(|agent| agent.agent_id == options.agent_id)
                .map(|agent| agent_json(agent, parsed.include_instructions));
            let extra = if parsed.include_instructions {
                Vec::new()
            } else {
                vec![("rawRedacted".to_string(), json!(true))]
            };
            let result = CliJsonResult::ok(
                "agents",
                Some(subcommand.to_string()),
                "resource_mutation",
                resource_mutation_json_vec(
                    "agent",
                    action,
                    Some(&options.agent_id.to_string()),
                    item,
                    extra,
                ),
                default_json_meta(parsed.debug, top_level_options.warnings()),
            );
            Ok(serde_json::to_string_pretty(&result)?)
        }
        Ok(updated) => {
            let summary = updated
                .iter()
                .find(|agent| agent.agent_id == options.agent_id)
                .map(agent_summary)
                .unwrap_or_else(|| format!("Agent {}", options.agent_id));
            Ok(format!("Updated agent {}\n{summary}", options.agent_id))
        }
        Err(error) if parsed.json => json_error(
            "agents",
            Some(subcommand.to_string()),
            error,
            "api_error",
            1,
        ),
        Err(error) => Ok(format!("Error: {error}")),
    }
}

async fn run_agent_edit(
    agent_id: i64,
    replace: bool,
    parsed: ParsedAgentsCommand,
    top_level_options: &options::GrokCommandOptions,
) -> Result<String> {
    let result = async {
        let client = configured_client(parsed.debug)?;
        let current = current_agent_customizations(&client, replace).await?;
        let agent = agent_customization(agent_id, &current)?;
        let edited = edit_agent_instructions(&agent)?;
        if edited == agent.instructions {
            return Ok::<_, anyhow::Error>((current, false));
        }
        let updated = merge_agent_customization(
            &AgentSetOptions {
                agent_id,
                name: None,
                instructions: edited,
                replace,
            },
            &current,
        )?;
        client.update_agent_customizations(&updated).await?;
        Ok((updated, true))
    }
    .await;

    match result {
        Ok((updated, changed)) if parsed.json => {
            let item = updated
                .iter()
                .find(|agent| agent.agent_id == agent_id)
                .map(|agent| agent_json(agent, parsed.include_instructions));
            let mut extra = vec![("changed".to_string(), json!(changed))];
            if !parsed.include_instructions {
                extra.push(("rawRedacted".to_string(), json!(true)));
            }
            let result = CliJsonResult::ok(
                "agents",
                Some("edit".to_string()),
                "resource_mutation",
                resource_mutation_json_vec(
                    "agent",
                    "edit",
                    Some(&agent_id.to_string()),
                    item,
                    extra,
                ),
                default_json_meta(parsed.debug, top_level_options.warnings()),
            );
            Ok(serde_json::to_string_pretty(&result)?)
        }
        Ok((_updated, false)) => Ok(format!("No changes made for agent {agent_id}.")),
        Ok((updated, true)) => {
            let summary = updated
                .iter()
                .find(|agent| agent.agent_id == agent_id)
                .map(agent_summary)
                .unwrap_or_else(|| format!("Agent {agent_id}"));
            Ok(format!("Updated agent {agent_id}\n{summary}"))
        }
        Err(error) if parsed.json => {
            json_error("agents", Some("edit".to_string()), error, "api_error", 1)
        }
        Err(error) => Ok(format!("Error: {error}")),
    }
}

fn parse_agents_command(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
) -> Result<ParsedAgentsCommand> {
    let (remaining, json, debug, replace, include_instructions) =
        parse_agent_global_options(args, top_level_options)?;
    let action = match remaining.first().map(String::as_str) {
        None => AgentsAction::List,
        Some(value) if router::is_help_argument(value) || value == "help" => {
            AgentsAction::Help(agents_usage())
        }
        Some("list") => {
            if remaining
                .iter()
                .skip(1)
                .any(|arg| router::is_help_argument(arg))
            {
                AgentsAction::Help(agents_list_usage())
            } else if remaining.len() == 1 {
                AgentsAction::List
            } else {
                anyhow::bail!("{}", agents_list_usage());
            }
        }
        Some("show" | "view") => {
            if remaining
                .iter()
                .skip(1)
                .any(|arg| router::is_help_argument(arg))
            {
                AgentsAction::Help(agents_show_usage())
            } else if remaining.len() == 2 {
                let agent_id = parse_agent_id(&remaining[1])?;
                AgentsAction::Show(agent_id)
            } else {
                anyhow::bail!("{}", agents_show_usage());
            }
        }
        Some("set") => {
            if remaining
                .iter()
                .skip(1)
                .any(|arg| router::is_help_argument(arg))
            {
                AgentsAction::Help(agents_set_usage())
            } else {
                AgentsAction::Set(parse_agent_set_options(&remaining[1..], replace)?)
            }
        }
        Some("edit") => {
            if remaining
                .iter()
                .skip(1)
                .any(|arg| router::is_help_argument(arg))
            {
                AgentsAction::Help(agents_edit_usage())
            } else if remaining.len() == 2 {
                AgentsAction::Edit {
                    agent_id: parse_agent_id(&remaining[1])?,
                    replace,
                }
            } else {
                anyhow::bail!("{}", agents_edit_usage());
            }
        }
        Some("clear") => {
            if remaining
                .iter()
                .skip(1)
                .any(|arg| router::is_help_argument(arg))
            {
                AgentsAction::Help(agents_clear_usage())
            } else if remaining.len() == 2 {
                AgentsAction::Clear(AgentSetOptions {
                    agent_id: parse_agent_id(&remaining[1])?,
                    name: None,
                    instructions: String::new(),
                    replace,
                })
            } else {
                anyhow::bail!("{}", agents_clear_usage());
            }
        }
        Some(command) => anyhow::bail!("Unknown agents command: {command}\n{}", agents_usage()),
    };

    Ok(ParsedAgentsCommand {
        action,
        json,
        debug,
        include_instructions,
    })
}

fn parse_agent_global_options(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
) -> Result<(Vec<String>, bool, bool, bool, bool)> {
    let mut remaining = Vec::new();
    let mut json = top_level_options.json;
    let mut debug = top_level_options.debug;
    let mut replace = false;
    let mut include_instructions = false;
    let mut index = 0;

    if let Some(format) = top_level_options.format.as_deref() {
        if format.trim().eq_ignore_ascii_case("json") {
            json = true;
        } else {
            anyhow::bail!("Invalid --format value: {format}. Use json.");
        }
    }

    while index < args.len() {
        let arg = &args[index];
        if arg == "--json" {
            json = true;
        } else if arg == "--format" {
            index += 1;
            let value = args
                .get(index)
                .filter(|value| !value.starts_with("--"))
                .ok_or_else(|| anyhow::anyhow!("--format requires a value"))?;
            if value.trim().eq_ignore_ascii_case("json") {
                json = true;
            } else {
                anyhow::bail!("Invalid --format value: {value}. Use json.");
            }
        } else if let Some(value) = arg.strip_prefix("--format=") {
            if value.is_empty() {
                anyhow::bail!("--format requires a value");
            }
            if value.trim().eq_ignore_ascii_case("json") {
                json = true;
            } else {
                anyhow::bail!("Invalid --format value: {value}. Use json.");
            }
        } else if arg == "--debug" {
            debug = true;
        } else if arg == "--replace" {
            replace = true;
        } else if arg == "--include-instructions" || arg == "--show-instructions" {
            include_instructions = true;
        } else {
            remaining.push(arg.clone());
        }
        index += 1;
    }

    Ok((remaining, json, debug, replace, include_instructions))
}

fn parse_agent_set_options(args: &[String], replace: bool) -> Result<AgentSetOptions> {
    let Some(first) = args.first() else {
        anyhow::bail!("Usage: {}", agents_set_usage());
    };
    let agent_id = parse_agent_id(first)?;
    let mut name = None;
    let mut instructions = None;
    let mut file_path = None;
    let mut index = 1;

    while index < args.len() {
        let arg = &args[index];
        if let Some(value) = arg.strip_prefix("--name=") {
            name = Some(required_inline_value("--name", value)?.to_string());
        } else if arg == "--name" {
            index += 1;
            name = Some(required_option_value(args, index, "--name")?.to_string());
        } else if let Some(value) = arg.strip_prefix("--instructions=") {
            instructions = Some(required_inline_value("--instructions", value)?.to_string());
        } else if arg == "--instructions" {
            index += 1;
            instructions = Some(required_option_value(args, index, "--instructions")?.to_string());
        } else if let Some(value) = arg
            .strip_prefix("--file=")
            .or_else(|| arg.strip_prefix("--instructions-file="))
        {
            file_path = Some(required_inline_value("--file", value)?.to_string());
        } else if arg == "--file" || arg == "--instructions-file" {
            index += 1;
            file_path = Some(required_option_value(args, index, arg)?.to_string());
        } else {
            anyhow::bail!(
                "Unknown option for agents set: {arg}\n{}",
                agents_set_usage()
            );
        }
        index += 1;
    }

    if instructions.is_some() && file_path.is_some() {
        anyhow::bail!("Use either --instructions or --file, not both");
    }
    let instructions = if let Some(instructions) = instructions {
        instructions
    } else if let Some(file_path) = file_path {
        std::fs::read_to_string(expand_tilde_path(PathBuf::from(&file_path)))
            .map_err(|error| anyhow::anyhow!("Could not read {file_path}: {error}"))?
    } else {
        anyhow::bail!(
            "Missing required option: --instructions <text> or --file <path>\n{}",
            agents_set_usage()
        );
    };

    Ok(AgentSetOptions {
        agent_id,
        name,
        instructions,
        replace,
    })
}

fn parse_agent_id(value: &str) -> Result<i64> {
    let agent_id = value.parse::<i64>().map_err(|_| {
        anyhow::anyhow!("Invalid agent ID. Grok currently exposes agent IDs 0, 1, 2, and 3.")
    })?;
    validate_agent_id(agent_id)?;
    Ok(agent_id)
}

fn validate_agent_id(agent_id: i64) -> Result<()> {
    if (0..=3).contains(&agent_id) {
        Ok(())
    } else {
        anyhow::bail!("Invalid agent ID. Grok currently exposes agent IDs 0, 1, 2, and 3.")
    }
}

async fn current_agent_customizations(
    client: &GrokClient,
    replace: bool,
) -> Result<Vec<GrokAgentCustomization>> {
    if replace {
        return Ok(default_agent_customizations());
    }
    let current = client
        .get_user_settings_response()
        .await?
        .agent_customizations;
    if current.is_empty() {
        anyhow::bail!(
            "User settings did not include agent values. Re-run with --replace to send a local four-agent profile."
        );
    }
    Ok(current)
}

fn merge_agent_customization(
    options: &AgentSetOptions,
    current: &[GrokAgentCustomization],
) -> Result<Vec<GrokAgentCustomization>> {
    validate_agent_id(options.agent_id)?;
    let mut by_id = default_agent_customizations()
        .into_iter()
        .map(|agent| (agent.agent_id, agent))
        .collect::<std::collections::BTreeMap<_, _>>();
    for agent in current {
        if (0..=3).contains(&agent.agent_id) {
            by_id.insert(agent.agent_id, agent.clone());
        }
    }
    let existing = by_id.get(&options.agent_id);
    let name = if options.agent_id == 0 {
        "Grok".to_string()
    } else {
        options
            .name
            .clone()
            .or_else(|| existing.map(|agent| agent.name.clone()))
            .unwrap_or_else(|| GrokAgentCustomization::default_name(options.agent_id))
    };
    by_id.insert(
        options.agent_id,
        GrokAgentCustomization::new(options.agent_id, name, options.instructions.clone()),
    );
    Ok(by_id.into_values().collect())
}

fn agent_customization(
    agent_id: i64,
    agents: &[GrokAgentCustomization],
) -> Result<GrokAgentCustomization> {
    validate_agent_id(agent_id)?;
    agents
        .iter()
        .find(|agent| agent.agent_id == agent_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Agent {agent_id} was not found in user settings."))
}

fn default_agent_customizations() -> Vec<GrokAgentCustomization> {
    (0..=3)
        .map(|agent_id| {
            GrokAgentCustomization::new(
                agent_id,
                GrokAgentCustomization::default_name(agent_id),
                "",
            )
        })
        .collect()
}

fn edit_agent_instructions(agent: &GrokAgentCustomization) -> Result<String> {
    let path = std::env::temp_dir().join(format!(
        "grok-agent-{}-instructions-{}.md",
        agent.agent_id,
        std::process::id()
    ));
    std::fs::write(&path, &agent.instructions)?;
    let editor = std::env::var("VISUAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| std::env::var("EDITOR").ok())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "vi".to_string());
    let mut parts = editor.split_whitespace().collect::<Vec<_>>();
    let executable = parts
        .first()
        .copied()
        .ok_or_else(|| anyhow::anyhow!("EDITOR is empty."))?;
    let mut command = if executable.contains('/') {
        ProcessCommand::new(expand_tilde_path(PathBuf::from(executable)))
    } else {
        let mut command = ProcessCommand::new("/usr/bin/env");
        command.arg(executable);
        command
    };
    for part in parts.drain(1..) {
        command.arg(part);
    }
    command.arg(&path);
    let status = command.status()?;
    if !status.success() {
        anyhow::bail!("Editor exited with status {}.", status.code().unwrap_or(-1));
    }
    let edited = std::fs::read_to_string(&path)?;
    let _ = std::fs::remove_file(&path);
    Ok(edited)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedTranscribeCommand {
    path: String,
    audio_format: Option<String>,
    refinement_level: String,
    json: bool,
    quiet: bool,
    debug: bool,
}

async fn run_transcribe_command(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
) -> Result<String> {
    if args.len() == 1 && router::is_help_argument(&args[0]) {
        return Ok(transcribe_usage());
    }

    let json_requested_on_error = top_level_options.json || is_json_requested(&args);
    let parsed = match parse_transcribe_command(args, top_level_options) {
        Ok(parsed) => parsed,
        Err(error) => {
            if json_requested_on_error {
                return json_error("transcribe", None, error, "usage_error", 2);
            }
            return Ok(format!("Error: {error}"));
        }
    };

    let resolved = match resolve_audio_input(&parsed) {
        Ok(resolved) => resolved,
        Err(error) => {
            if parsed.json {
                return json_error("transcribe", None, error, "usage_error", 2);
            }
            return Ok(format!("Error: {error}"));
        }
    };

    let shows_progress = !parsed.json && !parsed.quiet;
    let result = transcribe_resolved_audio_input(&parsed, resolved).await;

    match result {
        Ok((resolved, response)) if parsed.json => {
            let result = CliJsonResult::ok(
                "transcribe",
                None,
                "transcription",
                transcription_json(&resolved, &response),
                default_json_meta(parsed.debug, top_level_options.warnings()),
            );
            Ok(serde_json::to_string_pretty(&result)?)
        }
        Ok((_resolved, response)) if shows_progress => {
            Ok(format!("Transcribing audio...\n{}", response.text))
        }
        Ok((_resolved, response)) => Ok(response.text),
        Err(error) if parsed.json => json_error("transcribe", None, error, "api_error", 1),
        Err(error) if parsed.quiet => Ok(format!("Error: {error}")),
        Err(error) if shows_progress => Ok(format!("Transcribing audio...\nError: {error}")),
        Err(error) => Ok(format!("Error: {error}")),
    }
}

async fn transcribe_audio_input(
    parsed: &ParsedTranscribeCommand,
) -> Result<(ResolvedAudioInput, GrokSpeechToTextResponse)> {
    let resolved = resolve_audio_input(parsed)?;
    transcribe_resolved_audio_input(parsed, resolved).await
}

async fn transcribe_resolved_audio_input(
    parsed: &ParsedTranscribeCommand,
    resolved: ResolvedAudioInput,
) -> Result<(ResolvedAudioInput, GrokSpeechToTextResponse)> {
    let client = configured_client(parsed.debug)?;
    let response = client
        .speech_to_text_response(
            &resolved.audio_base64,
            &GrokSpeechToTextOptions {
                audio_format: Some(resolved.audio_format.clone()),
                refinement_level: parsed.refinement_level.clone(),
            },
        )
        .await?;
    Ok((resolved, response))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedAudioInput {
    source_path: String,
    audio_format: String,
    refinement_level: String,
    audio_base64: String,
}

fn parse_transcribe_command(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
) -> Result<ParsedTranscribeCommand> {
    let mut audio_args = Vec::new();
    let mut json = top_level_options.json
        || top_level_options
            .format
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("json"));
    let mut debug = top_level_options.debug;
    let mut quiet = false;
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        if arg == "--json" {
            json = true;
        } else if arg == "--raw" {
            json = false;
        } else if arg == "--markdown" || arg == "-m" {
            anyhow::bail!("Invalid output format 'markdown'. Use raw or json.");
        } else if let Some(value) = arg.strip_prefix("--format=") {
            apply_transcribe_format_value(value, &mut json)?;
        } else if arg == "--format" {
            index += 1;
            let value = args
                .get(index)
                .ok_or_else(|| anyhow::anyhow!("--format requires a format value"))?;
            apply_transcribe_format_value(value, &mut json)?;
        } else if arg == "--debug" {
            debug = true;
        } else if arg == "--quiet" {
            quiet = true;
        } else {
            audio_args.push(arg.clone());
        }
        index += 1;
    }

    let (path, audio_format, refinement_level) = parse_audio_input_options(&audio_args)?;
    Ok(ParsedTranscribeCommand {
        path,
        audio_format,
        refinement_level,
        json,
        quiet,
        debug,
    })
}

fn apply_transcribe_format_value(value: &str, json: &mut bool) -> Result<()> {
    match options::OutputFormat::resolve(value) {
        Some(options::OutputFormat::Json) => {
            *json = true;
            Ok(())
        }
        Some(options::OutputFormat::Raw) => {
            *json = false;
            Ok(())
        }
        Some(options::OutputFormat::Markdown) => {
            anyhow::bail!("Invalid output format 'markdown'. Use raw or json.")
        }
        None => anyhow::bail!("Invalid output format '{value}'. Use raw or json."),
    }
}

fn resolve_audio_input(parsed: &ParsedTranscribeCommand) -> Result<ResolvedAudioInput> {
    let (audio_data, file_name) = if parsed.path == "-" {
        let mut data = Vec::new();
        std::io::stdin().read_to_end(&mut data)?;
        (data, None)
    } else {
        let expanded_path = expand_tilde_path(PathBuf::from(&parsed.path));
        let data = std::fs::read(&expanded_path).map_err(|error| {
            anyhow::anyhow!("Could not read audio file {}: {error}", parsed.path)
        })?;
        let file_name = expanded_path
            .file_name()
            .map(|value| value.to_string_lossy().to_string());
        (data, file_name)
    };

    if audio_data.is_empty() {
        if parsed.path == "-" {
            anyhow::bail!("Please provide audio bytes on stdin");
        }
        anyhow::bail!("Audio file is empty: {}", parsed.path);
    }

    let audio_format = parsed
        .audio_format
        .clone()
        .or_else(|| file_name.as_deref().and_then(infer_audio_format).map(str::to_string))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Could not infer audio format. Pass --audio-format when using stdin or an unknown extension."
            )
        })?;

    Ok(ResolvedAudioInput {
        source_path: parsed.path.clone(),
        audio_format,
        refinement_level: parsed.refinement_level.clone(),
        audio_base64: general_purpose::STANDARD.encode(audio_data),
    })
}

fn transcription_json(resolved: &ResolvedAudioInput, response: &GrokSpeechToTextResponse) -> Value {
    json!({
        "kind": "audio",
        "transcript": response.text,
        "audio": {
            "path": resolved.source_path,
            "format": resolved.audio_format,
            "refinementLevel": resolved.refinement_level
        }
    })
}

fn configured_client(debug: bool) -> Result<GrokClient> {
    if let Ok(credentials) = config::load_credentials() {
        return Ok(GrokClient::with_options(credentials, debug, None)?);
    }

    Err(GrokError::InvalidCredentials.into())
}

fn json_error(
    command: &str,
    subcommand: Option<String>,
    error: anyhow::Error,
    default_code: &str,
    exit_code: i32,
) -> Result<String> {
    let details = json_error_details(&error, default_code);
    let result = CliJsonResult::error(
        command,
        subcommand,
        details.message,
        details.code,
        exit_code,
        details.recoverable,
        Some(details.raw_message),
    );
    Ok(serde_json::to_string_pretty(&result)?)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct JsonErrorDetails {
    code: String,
    message: String,
    recoverable: bool,
    raw_message: String,
}

fn json_error_details(error: &anyhow::Error, default_code: &str) -> JsonErrorDetails {
    let raw_message = error.to_string();
    if default_code == "usage_error" {
        return JsonErrorDetails {
            code: default_code.to_string(),
            message: raw_message.clone(),
            recoverable: false,
            raw_message,
        };
    }

    if let Some(grok_error) = error.downcast_ref::<GrokError>() {
        let code = match grok_error {
            GrokError::InvalidCredentials | GrokError::Unauthorized => "auth_error",
            GrokError::AccessDenied(_) => "access_denied",
            GrokError::Network(_) | GrokError::Url(_) => "network_error",
            GrokError::Decoding(_) => "decoding_error",
            GrokError::NotFound(_) => "not_found",
            GrokError::Api(_) => {
                if rate_limit_json_message(&raw_message).is_some() {
                    "rate_limit"
                } else {
                    default_code
                }
            }
            GrokError::Streaming(_) => "streaming_error",
        };
        let message = rate_limit_json_message(&raw_message).unwrap_or_else(|| raw_message.clone());
        return JsonErrorDetails {
            code: code.to_string(),
            message,
            recoverable: matches!(
                grok_error,
                GrokError::InvalidCredentials | GrokError::Unauthorized
            ),
            raw_message,
        };
    }

    let message = rate_limit_json_message(&raw_message).unwrap_or_else(|| raw_message.clone());
    JsonErrorDetails {
        code: if rate_limit_json_message(&raw_message).is_some() {
            "rate_limit".to_string()
        } else {
            default_code.to_string()
        },
        message,
        recoverable: false,
        raw_message,
    }
}

fn rate_limit_json_message(raw_message: &str) -> Option<String> {
    let normalized = raw_message.to_lowercase();
    let is_rate_limited = normalized.contains("http error: 429")
        || (normalized.contains("too many requests") && normalized.contains("\"code\":8"));
    if !is_rate_limited {
        return None;
    }

    if let Some(wait_time) = wait_time_hint(raw_message) {
        return Some(format!(
            "Message limit reached. Grok is rate limiting this account right now. Wait {wait_time}, then try again."
        ));
    }

    Some(
        "Message limit reached. Grok is rate limiting this account right now. Wait a few minutes, then try again. The Grok web app may show the exact reset time for your plan."
            .to_string(),
    )
}

fn wait_time_hint(message: &str) -> Option<String> {
    let words = message
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();

    for window in words.windows(3) {
        if !window[0].eq_ignore_ascii_case("wait") {
            continue;
        }
        let Ok(number) = window[1].parse::<u64>() else {
            continue;
        };
        let unit = window[2].to_lowercase();
        let singular_unit = match unit.as_str() {
            "second" | "seconds" => "second",
            "minute" | "minutes" => "minute",
            "hour" | "hours" => "hour",
            _ => continue,
        };
        let suffix = if number == 1 { "" } else { "s" };
        return Some(format!("{number} {singular_unit}{suffix}"));
    }

    None
}

fn resource_list_json<const N: usize>(
    resource: &str,
    items: Vec<Value>,
    extra: [(String, Value); N],
) -> Value {
    let mut data = Map::new();
    data.insert("resource".to_string(), json!(resource));
    data.insert("items".to_string(), Value::Array(items));
    for (key, value) in extra {
        data.insert(key, value);
    }
    Value::Object(data)
}

fn resource_list_json_vec(resource: &str, items: Vec<Value>, extra: Vec<(String, Value)>) -> Value {
    let mut data = Map::new();
    data.insert("resource".to_string(), json!(resource));
    data.insert("items".to_string(), Value::Array(items));
    for (key, value) in extra {
        data.insert(key, value);
    }
    Value::Object(data)
}

fn resource_mutation_json<const N: usize>(
    resource: &str,
    action: &str,
    id: Option<&str>,
    item: Option<Value>,
    extra: [(String, Value); N],
) -> Value {
    let mut data = Map::new();
    data.insert("resource".to_string(), json!(resource));
    data.insert("action".to_string(), json!(action));
    data.insert("completed".to_string(), json!(true));
    if let Some(id) = id {
        data.insert("id".to_string(), json!(id));
    }
    if let Some(item) = item {
        data.insert("item".to_string(), item);
    }
    for (key, value) in extra {
        data.insert(key, value);
    }
    Value::Object(data)
}

fn resource_mutation_json_vec(
    resource: &str,
    action: &str,
    id: Option<&str>,
    item: Option<Value>,
    extra: Vec<(String, Value)>,
) -> Value {
    let mut data = Map::new();
    data.insert("resource".to_string(), json!(resource));
    data.insert("action".to_string(), json!(action));
    data.insert("completed".to_string(), json!(true));
    if let Some(id) = id {
        data.insert("id".to_string(), json!(id));
    }
    if let Some(item) = item {
        data.insert("item".to_string(), item);
    }
    for (key, value) in extra {
        data.insert(key, value);
    }
    Value::Object(data)
}

fn upload_response_json(response: &GrokFileUploadResponse) -> Value {
    let mut data = Map::new();
    if let Some(id) = response.uploaded_file_id() {
        data.insert("id".to_string(), json!(id));
    }
    if let Some(file_name) = response.file_name.as_deref() {
        data.insert("fileName".to_string(), json!(file_name));
    }
    if let Some(asset) = response.asset.as_ref() {
        data.insert("asset".to_string(), asset_json(asset));
    }
    Value::Object(data)
}

fn conversations_json(conversations: &[GrokConversation]) -> Value {
    json!({
        "conversations": conversations.iter().map(conversation_json).collect::<Vec<_>>()
    })
}

fn assistant_response_json(
    response: &grok_client::ConversationResponse,
    mode: &GrokMode,
    request: Value,
    input: Option<Value>,
) -> Value {
    let visible_message = GrokStreamMarkupParser::visible_text(&response.message, true);
    let web_search_results = response
        .web_search_results
        .as_ref()
        .map(|items| json!(items))
        .unwrap_or_else(|| json!([]));
    let xposts = response
        .xposts
        .as_ref()
        .map(|items| json!(items))
        .unwrap_or_else(|| json!([]));

    let mut data = json!({
    "message": visible_message.clone(),
    "conversationId": response.conversation_id,
    "responseId": response.response_id,
    "model": mode_json(mode),
    "sources": {
        "webSearchResults": web_search_results,
        "xposts": xposts
    },
        "request": request
    });
    if visible_message != response.message
        && let Some(object) = data.as_object_mut()
    {
        object.insert("rawMessage".to_string(), json!(response.message));
    }
    if let Some(input) = input
        && let Some(object) = data.as_object_mut()
    {
        object.insert("input".to_string(), input);
    }
    data
}

fn message_request_json(parsed: &ParsedMessageCommand) -> Value {
    json!({
        "reasoning": true,
        "deepSearch": false,
        "noSearch": false,
        "private": parsed.private_mode,
        "stream": parsed.stream,
        "workspaceIds": [],
        "fileAttachmentIds": parsed.file_attachment_ids
    })
}

fn message_audio_input_metadata(
    resolved: &ResolvedAudioInput,
    response: &GrokSpeechToTextResponse,
) -> MessageAudioInputMetadata {
    MessageAudioInputMetadata {
        transcript: response.text.clone(),
        source_path: resolved.source_path.clone(),
        audio_format: resolved.audio_format.clone(),
        refinement_level: resolved.refinement_level.clone(),
    }
}

fn message_audio_input_json(parsed: &ParsedMessageCommand) -> Option<Value> {
    parsed.resolved_audio_input.as_ref().map(|input| {
        json!({
            "kind": "audio",
            "transcript": input.transcript,
            "audio": {
                "path": input.source_path,
                "format": input.audio_format,
                "refinementLevel": input.refinement_level
            }
        })
    })
}

fn streaming_message_json(stream_text: &str, parsed: &ParsedMessageCommand) -> Result<String> {
    let mut events = Vec::new();
    let mut sequence = 1;
    let request = message_request_json(parsed);
    let warnings = parsed.warnings.clone();
    let mut request_data = json!({
        "message": parsed.message,
        "model": mode_json(&parsed.selected_mode),
        "request": request
    });
    let input = message_audio_input_json(parsed);
    if let Some(input) = input.as_ref() {
        push_json_event(&mut events, &mut sequence, "transcription", input.clone())?;
        if let Some(object) = request_data.as_object_mut() {
            object.insert("input".to_string(), input.clone());
        }
    }
    if !warnings.is_empty()
        && let Some(object) = request_data.as_object_mut()
    {
        object.insert("warnings".to_string(), json!(warnings));
    }
    push_json_event(&mut events, &mut sequence, "request", request_data)?;
    push_json_event(
        &mut events,
        &mut sequence,
        "progress",
        json!({
            "phase": "stream_started",
            "message": "Streaming response started"
        }),
    )?;

    let mut parser = GrokStreamParser::new("");
    let mut answer_parser = GrokStreamMarkupParser::new();
    let mut thinking_parser = GrokStreamMarkupParser::new();
    let mut thinking_active = false;
    let mut emitted_final = false;
    for line in stream_text.lines() {
        let Some(response) = parser.consume_line(line)? else {
            continue;
        };
        if response.is_soft_stop && response.message.is_empty() {
            continue;
        }
        if response.is_final {
            push_thinking_display_events(
                &mut events,
                &mut sequence,
                thinking_parser.finish(),
                &mut thinking_active,
            )?;
            push_thinking_end_if_needed(&mut events, &mut sequence, &mut thinking_active)?;
            push_display_events(
                &mut events,
                &mut sequence,
                answer_parser.finish(),
                "assistant_delta",
            )?;
            push_json_event(
                &mut events,
                &mut sequence,
                "assistant_final",
                assistant_response_json(
                    &response,
                    &parsed.selected_mode,
                    request.clone(),
                    input.clone(),
                ),
            )?;
            emitted_final = true;
            break;
        }

        if response.is_thinking {
            push_thinking_display_events(
                &mut events,
                &mut sequence,
                thinking_parser.consume(&response.message),
                &mut thinking_active,
            )?;
        } else {
            push_thinking_display_events(
                &mut events,
                &mut sequence,
                thinking_parser.finish(),
                &mut thinking_active,
            )?;
            push_thinking_end_if_needed(&mut events, &mut sequence, &mut thinking_active)?;
            push_display_events(
                &mut events,
                &mut sequence,
                answer_parser.consume(&response.message),
                "assistant_delta",
            )?;
        }
    }

    if !emitted_final && let Some(response) = parser.finish() {
        push_thinking_display_events(
            &mut events,
            &mut sequence,
            thinking_parser.finish(),
            &mut thinking_active,
        )?;
        push_thinking_end_if_needed(&mut events, &mut sequence, &mut thinking_active)?;
        push_display_events(
            &mut events,
            &mut sequence,
            answer_parser.finish(),
            "assistant_delta",
        )?;
        push_json_event(
            &mut events,
            &mut sequence,
            "assistant_final",
            assistant_response_json(&response, &parsed.selected_mode, request, input),
        )?;
        emitted_final = true;
    }

    if !emitted_final {
        push_thinking_display_events(
            &mut events,
            &mut sequence,
            thinking_parser.finish(),
            &mut thinking_active,
        )?;
        push_thinking_end_if_needed(&mut events, &mut sequence, &mut thinking_active)?;
        push_display_events(
            &mut events,
            &mut sequence,
            answer_parser.finish(),
            "assistant_delta",
        )?;
    }

    push_json_event(
        &mut events,
        &mut sequence,
        "done",
        json!({
            "ok": emitted_final,
            "phase": if emitted_final { "complete" } else { "aborted" }
        }),
    )?;
    Ok(events.join("\n"))
}

fn push_display_events(
    events: &mut Vec<String>,
    sequence: &mut usize,
    display_events: Vec<StreamDisplayEvent>,
    text_event: &str,
) -> Result<()> {
    for event in display_events {
        match event {
            StreamDisplayEvent::Text(text) => {
                if !text.is_empty() {
                    push_json_event(events, sequence, text_event, json!({ "text": text }))?;
                }
            }
            StreamDisplayEvent::Activity(activity) => {
                let kind = activity.kind.as_str();
                let text = activity.display_text();
                push_json_event(
                    events,
                    sequence,
                    "activity",
                    json!({ "kind": kind, "text": text.clone() }),
                )?;
                push_json_event(
                    events,
                    sequence,
                    "trace",
                    json!({ "kind": kind, "text": text }),
                )?;
            }
        }
    }
    Ok(())
}

fn push_thinking_display_events(
    events: &mut Vec<String>,
    sequence: &mut usize,
    display_events: Vec<StreamDisplayEvent>,
    thinking_active: &mut bool,
) -> Result<()> {
    for event in display_events {
        match event {
            StreamDisplayEvent::Text(text) => {
                for line in text
                    .split('\n')
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                {
                    push_thinking_start_if_needed(events, sequence, thinking_active)?;
                    push_json_event(events, sequence, "thinking_delta", json!({ "text": line }))?;
                    push_json_event(
                        events,
                        sequence,
                        "trace",
                        json!({ "kind": "thinking", "text": line }),
                    )?;
                }
            }
            StreamDisplayEvent::Activity(activity) => {
                let kind = activity.kind.as_str();
                let text = activity.display_text();
                push_json_event(
                    events,
                    sequence,
                    "activity",
                    json!({ "kind": kind, "text": text.clone() }),
                )?;
                push_json_event(
                    events,
                    sequence,
                    "trace",
                    json!({ "kind": kind, "text": text }),
                )?;
            }
        }
    }
    Ok(())
}

fn push_thinking_start_if_needed(
    events: &mut Vec<String>,
    sequence: &mut usize,
    thinking_active: &mut bool,
) -> Result<()> {
    if !*thinking_active {
        push_json_event(
            events,
            sequence,
            "thinking_start",
            json!({ "phase": "thinking" }),
        )?;
        *thinking_active = true;
    }
    Ok(())
}

fn push_thinking_end_if_needed(
    events: &mut Vec<String>,
    sequence: &mut usize,
    thinking_active: &mut bool,
) -> Result<()> {
    if *thinking_active {
        push_json_event(
            events,
            sequence,
            "thinking_end",
            json!({ "phase": "thinking" }),
        )?;
        *thinking_active = false;
    }
    Ok(())
}

fn push_json_event(
    events: &mut Vec<String>,
    sequence: &mut usize,
    event: &str,
    data: Value,
) -> Result<()> {
    events.push(serde_json::to_string(&json!({
        "schema": "grok.cli.event.v1",
        "sequence": *sequence,
        "event": event,
        "data": data
    }))?);
    *sequence += 1;
    Ok(())
}

fn mode_json(mode: &GrokMode) -> Value {
    let mut data = Map::new();
    data.insert("id".to_string(), json!(mode.id));
    data.insert("displayName".to_string(), json!(mode.display_name));
    data.insert("summary".to_string(), json!(mode.summary));
    data.insert("available".to_string(), json!(mode.is_available));
    data.insert("disabled".to_string(), json!(!mode.is_available));
    if let Some(unavailable_reason) = mode.unavailable_reason.as_deref() {
        data.insert("unavailableReason".to_string(), json!(unavailable_reason));
    }
    if let Some(minimum_subscription_tier) = mode.minimum_subscription_tier.as_deref() {
        data.insert(
            "minimumSubscriptionTier".to_string(),
            json!(minimum_subscription_tier),
        );
    }
    Value::Object(data)
}

fn conversation_json(conversation: &GrokConversation) -> Value {
    json!({
        "conversationId": conversation.conversation_id,
        "title": conversation.title,
        "starred": conversation.starred,
        "createTime": conversation.create_time,
        "modifyTime": conversation.modify_time,
        "temporary": conversation.temporary,
        "mediaTypes": conversation.media_types
    })
}

fn conversation_history_json(
    conversation_id: &str,
    responses: &[GrokConversationMessage],
) -> Value {
    json!({
        "conversationId": conversation_id,
        "responses": responses.iter().map(conversation_message_json).collect::<Vec<_>>()
    })
}

fn conversation_message_json(response: &GrokConversationMessage) -> Value {
    let mut data = Map::new();
    data.insert("responseId".to_string(), json!(response.response_id));
    data.insert("sender".to_string(), json!(response.sender));
    data.insert("message".to_string(), json!(response.message));
    data.insert("createTime".to_string(), json!(response.create_time));
    if let Some(parent_response_id) = response.parent_response_id.as_deref() {
        data.insert("parentResponseId".to_string(), json!(parent_response_id));
    }
    Value::Object(data)
}

fn asset_json(asset: &GrokAsset) -> Value {
    let mut data = Map::new();
    if let Some(id) = asset.resolved_id() {
        data.insert("id".to_string(), json!(id));
    }
    if let Some(file_name) = asset.display_file_name() {
        data.insert("fileName".to_string(), json!(file_name));
    }
    if let Some(mime_type) = asset.mime_type.as_deref() {
        data.insert("mimeType".to_string(), json!(mime_type));
    }
    Value::Object(data)
}

fn workspace_json(workspace: &GrokWorkspace) -> Value {
    let mut data = Map::new();
    if let Some(id) = workspace.resolved_id() {
        data.insert("id".to_string(), json!(id));
    }
    if let Some(workspace_id) = workspace.workspace_id.as_deref() {
        data.insert("workspaceId".to_string(), json!(workspace_id));
    }
    if let Some(name) = workspace.display_name() {
        data.insert("name".to_string(), json!(name));
    }
    if let Some(title) = workspace.title.as_deref() {
        data.insert("title".to_string(), json!(title));
    }
    if let Some(icon) = workspace.icon.as_deref() {
        data.insert("icon".to_string(), json!(icon));
    }
    if let Some(preferred_model) = workspace.preferred_model.as_deref() {
        data.insert("preferredModel".to_string(), json!(preferred_model));
    }
    if let Some(custom_personality) = workspace.custom_personality.as_deref() {
        data.insert("customPersonality".to_string(), json!(custom_personality));
    }
    Value::Object(data)
}

fn task_json(task: &GrokTask) -> Value {
    let mut data = Map::new();
    if let Some(id) = task_identifier(task) {
        data.insert("id".to_string(), json!(id));
    }
    if let Some(task_id) = task
        .task_id
        .clone()
        .or_else(|| string_in_value(&task.raw_json, &["taskId", "task_id"]))
    {
        data.insert("taskId".to_string(), json!(task_id));
    }
    if let Some(name) = task
        .name
        .clone()
        .or_else(|| string_in_value(&task.raw_json, &["name", "title"]))
    {
        data.insert("name".to_string(), json!(name));
    }
    if let Some(prompt) = task
        .prompt
        .clone()
        .or_else(|| string_in_value(&task.raw_json, &["prompt"]))
    {
        data.insert("prompt".to_string(), json!(prompt));
    }
    if let Some(is_enabled) = task
        .is_enabled
        .or_else(|| bool_in_value(&task.raw_json, &["isEnabled", "is_enabled"]))
    {
        data.insert("isEnabled".to_string(), json!(is_enabled));
    }
    if let Some(schedule_is_enabled) = schedule_enabled(&task.raw_json) {
        data.insert("scheduleIsEnabled".to_string(), json!(schedule_is_enabled));
    }
    if let Some(status) = task_status(task) {
        data.insert("status".to_string(), json!(status));
    }
    if let Some(schedule) = schedule_value(&task.raw_json) {
        data.insert("schedule".to_string(), json!(schedule));
    }
    Value::Object(data)
}

fn task_detail_json(task: &GrokTask, latest_result: Option<&GrokTaskResult>) -> Value {
    let mut data = task_json(task).as_object().cloned().unwrap_or_default();
    if let Some(title) = task_title(task) {
        data.insert("title".to_string(), json!(title));
    }
    if let Some(latest_result) = latest_result {
        data.insert("latestResult".to_string(), task_result_json(latest_result));
    } else if let Some(latest_result) = latest_task_result_value(&task.raw_json) {
        data.insert("latestResult".to_string(), latest_result);
    }
    Value::Object(data)
}

fn task_result_json(result: &GrokTaskResult) -> Value {
    let mut data = Map::new();
    if let Some(id) = result.resolved_id().map(ToOwned::to_owned).or_else(|| {
        string_in_value(
            &result.raw_json,
            &[
                "taskResultId",
                "task_result_id",
                "resultId",
                "result_id",
                "id",
            ],
        )
    }) {
        data.insert("id".to_string(), json!(id));
    }
    if let Some(result_id) = result.result_id.clone().or_else(|| {
        string_in_value(
            &result.raw_json,
            &["taskResultId", "task_result_id", "resultId", "result_id"],
        )
    }) {
        data.insert("resultId".to_string(), json!(result_id));
    }
    if let Some(task_id) = result
        .task_id
        .clone()
        .or_else(|| string_in_value(&result.raw_json, &["taskId", "task_id"]))
    {
        data.insert("taskId".to_string(), json!(task_id));
    }
    if let Some(conversation_id) = result
        .conversation_id
        .clone()
        .or_else(|| string_in_value(&result.raw_json, &["conversationId", "conversation_id"]))
    {
        data.insert("conversationId".to_string(), json!(conversation_id));
    }
    if let Some(response_id) = result
        .response_id
        .clone()
        .or_else(|| string_in_value(&result.raw_json, &["responseId", "response_id"]))
    {
        data.insert("responseId".to_string(), json!(response_id));
    }
    if let Some(message) = result.message.clone().or_else(|| {
        string_in_value(
            &result.raw_json,
            &["summary", "message", "content", "output", "text", "result"],
        )
    }) {
        data.insert("message".to_string(), json!(message));
    }
    if let Some(status) = result
        .status
        .clone()
        .or_else(|| string_in_value(&result.raw_json, &["status", "state"]))
    {
        data.insert("status".to_string(), json!(status));
    }
    if let Some(created) = task_result_timestamp(result) {
        data.insert("created".to_string(), json!(created));
    }
    Value::Object(data)
}

fn skill_json(skill: &GrokSkill) -> Value {
    let mut data = Map::new();
    if let Some(id) = skill.resolved_id() {
        data.insert("id".to_string(), json!(id));
    }
    if let Some(skill_id) = skill.skill_id.as_deref() {
        data.insert("skillId".to_string(), json!(skill_id));
    }
    if let Some(name) = skill.display_name() {
        data.insert("name".to_string(), json!(name));
    }
    if let Some(title) = skill.title.as_deref() {
        data.insert("title".to_string(), json!(title));
    }
    if let Some(status) = skill.status.as_deref() {
        data.insert("status".to_string(), json!(status));
    }
    if let Some(description) = skill.description.as_deref() {
        data.insert("description".to_string(), json!(description));
    }
    Value::Object(data)
}

fn agent_json(agent: &GrokAgentCustomization, include_instructions: bool) -> Value {
    let mut data = Map::new();
    data.insert("agentId".to_string(), json!(agent.agent_id));
    data.insert("name".to_string(), json!(agent.name));
    data.insert(
        "instructionLength".to_string(),
        json!(agent.instructions.chars().count()),
    );
    data.insert(
        "instructionsRedacted".to_string(),
        json!(!include_instructions),
    );
    if include_instructions {
        data.insert("instructions".to_string(), json!(agent.instructions));
    }
    Value::Object(data)
}

fn conversation_v2_json(response: &GrokConversationV2Response, fallback_id: &str) -> Value {
    let conversation = conversation_detail_value(&response.raw_json);
    let id = response
        .conversation_id
        .as_deref()
        .map(ToOwned::to_owned)
        .or_else(|| {
            first_string_in_value(conversation, &["conversationId", "conversation_id", "id"])
        })
        .unwrap_or_else(|| fallback_id.to_string());
    let title = first_string_in_value(conversation, &["title", "name"]);
    let workspace_count = first_array_count_in_value(
        conversation,
        &["workspaces", "workspaceIds", "workspace_ids"],
    );
    let has_task_result = contains_any_key_in_value(conversation, &["taskResult", "task_result"]);

    let mut data = Map::new();
    data.insert("conversationId".to_string(), json!(id));
    data.insert("hasTaskResult".to_string(), json!(has_task_result));
    if let Some(title) = title {
        data.insert("title".to_string(), json!(title));
    }
    if let Some(workspace_count) = workspace_count {
        data.insert("workspaceCount".to_string(), json!(workspace_count));
    }
    Value::Object(data)
}

fn conversation_detail_value(raw_json: &Value) -> &Value {
    raw_json
        .as_object()
        .and_then(|dictionary| {
            ["conversation", "data", "result"]
                .into_iter()
                .find_map(|key| dictionary.get(key).filter(|value| value.is_object()))
        })
        .unwrap_or(raw_json)
}

fn first_string_in_value(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(dictionary) => {
            for key in keys {
                if let Some(text) = dictionary.get(*key).and_then(Value::as_str)
                    && !text.is_empty()
                {
                    return Some(text.to_string());
                }
            }
            dictionary
                .values()
                .find_map(|nested| first_string_in_value(nested, keys))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|nested| first_string_in_value(nested, keys)),
        _ => None,
    }
}

fn first_array_count_in_value(value: &Value, keys: &[&str]) -> Option<usize> {
    match value {
        Value::Object(dictionary) => {
            for key in keys {
                if let Some(array) = dictionary.get(*key).and_then(Value::as_array) {
                    return Some(array.len());
                }
            }
            dictionary
                .values()
                .find_map(|nested| first_array_count_in_value(nested, keys))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|nested| first_array_count_in_value(nested, keys)),
        _ => None,
    }
}

fn contains_any_key_in_value(value: &Value, keys: &[&str]) -> bool {
    match value {
        Value::Object(dictionary) => {
            keys.iter().any(|key| dictionary.contains_key(*key))
                || dictionary
                    .values()
                    .any(|nested| contains_any_key_in_value(nested, keys))
        }
        Value::Array(values) => values
            .iter()
            .any(|nested| contains_any_key_in_value(nested, keys)),
        _ => false,
    }
}

fn conversation_rows(conversations: &[GrokConversation]) -> String {
    if conversations.is_empty() {
        return "No conversations found.".to_string();
    }

    let mut lines = vec!["Available conversations:".to_string()];
    for (index, conversation) in conversations.iter().enumerate() {
        lines.push(format!("{}. {}", index + 1, conversation.title));
    }
    lines.join("\n")
}

fn conversation_history_rows(responses: &[GrokConversationMessage]) -> String {
    if responses.is_empty() {
        return "This conversation has no messages yet.".to_string();
    }

    responses
        .iter()
        .map(|response| {
            let sender = if response.sender == "human" {
                "User"
            } else {
                "Grok"
            };
            format!("{sender}: {}", response.message)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn asset_rows(assets: &[GrokAsset]) -> String {
    if assets.is_empty() {
        return "No files found.".to_string();
    }

    let mut lines = vec!["Files:".to_string()];
    for asset in assets {
        let id = asset.resolved_id().unwrap_or("");
        let file_name = asset.display_file_name().unwrap_or("");
        let mime_type = asset.mime_type.as_deref().unwrap_or("");
        lines.push(format!("ID: {id}"));
        lines.push(format!("File: {file_name}"));
        lines.push(format!("MIME: {mime_type}"));
    }
    lines.join("\n")
}

fn workspace_rows(workspaces: &[GrokWorkspace]) -> String {
    if workspaces.is_empty() {
        return "No workspaces found.".to_string();
    }

    let mut lines = vec!["Workspaces:".to_string()];
    for workspace in workspaces {
        lines.push(workspace_summary(workspace));
    }
    lines.join("\n")
}

fn skill_sections(built_in_skills: &[GrokSkill], user_skills: &[GrokSkill]) -> String {
    let mut sections = vec![skill_rows("Grok Skills", built_in_skills)];
    if !user_skills.is_empty() {
        sections.push(skill_rows("User Skills", user_skills));
    }
    sections.join("\n\n")
}

fn skill_rows(title: &str, skills: &[GrokSkill]) -> String {
    if skills.is_empty() {
        return "No skills found.".to_string();
    }

    let mut lines = vec![title.to_string()];
    for skill in skills {
        let mut parts = Vec::new();
        if let Some(name) = skill.display_name() {
            parts.push(name.to_string());
        }
        if let Some(description) = skill.description.as_deref()
            && !description.is_empty()
        {
            parts.push(description.to_string());
        }
        lines.push(if parts.is_empty() {
            "(no summary available)".to_string()
        } else {
            parts.join(" | ")
        });
    }
    lines.join("\n")
}

fn task_rows(tasks: &[GrokTask]) -> String {
    if tasks.is_empty() {
        return "No tasks found.".to_string();
    }

    let mut lines = vec!["Tasks".to_string()];
    for task in tasks {
        lines.push(task_summary(task));
    }
    lines.join("\n")
}

fn task_summary(task: &GrokTask) -> String {
    let parts = [
        task_title(task),
        task_status(task),
        schedule_value(&task.raw_json),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if parts.is_empty() {
        "(no summary available)".to_string()
    } else {
        parts.join("  ")
    }
}

fn task_detail(task: &GrokTask, latest_result: Option<&GrokTaskResult>) -> String {
    let mut lines = vec![task_summary(task)];
    if let Some(prompt) = task_prompt(task) {
        lines.push(String::new());
        lines.push(prompt);
    }
    lines.push(String::new());
    lines.push(task_result_block(latest_result));
    lines.join("\n")
}

fn task_result_rows(task_id: &str, results: &[GrokTaskResult]) -> String {
    if results.is_empty() {
        return format!("No task runs found for {task_id}.");
    }
    if results.len() == 1 {
        return task_result_block(results.first());
    }

    let mut lines = vec!["Task runs".to_string()];
    for (index, result) in results.iter().enumerate() {
        let title = [
            Some(run_ordinal_label(index)),
            task_result_timestamp(result).map(|timestamp| task_run_timestamp_label(&timestamp)),
            result.status.as_deref().and_then(compact_task_status),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("  ");
        lines.push(title);
        if let Some(message) = task_result_message(result)
            && !message.trim().is_empty()
        {
            lines.push(message);
        }
        if index != results.len() - 1 {
            lines.push(String::new());
        }
    }
    lines.join("\n")
}

fn task_result_block(result: Option<&GrokTaskResult>) -> String {
    let Some(result) = result else {
        return "Latest run  none".to_string();
    };
    let summary = [
        Some("Latest run".to_string()),
        result.status.as_deref().and_then(compact_task_status),
        task_result_timestamp(result).map(compact_timestamp),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("  ");
    if let Some(message) = task_result_message(result)
        && !message.trim().is_empty()
    {
        return format!("{summary}\n{message}");
    }
    summary
}

fn task_identifier(task: &GrokTask) -> Option<String> {
    task.task_id
        .clone()
        .or_else(|| task.id.clone())
        .or_else(|| string_in_value(&task.raw_json, &["taskId", "task_id", "id"]))
}

fn task_title(task: &GrokTask) -> Option<String> {
    task.name
        .clone()
        .or_else(|| string_in_value(&task.raw_json, &["title", "name", "displayName"]))
        .or_else(|| task.prompt.clone())
        .or_else(|| {
            string_in_value(
                &task.raw_json,
                &["prompt", "taskPrompt", "description", "summary"],
            )
        })
}

fn task_prompt(task: &GrokTask) -> Option<String> {
    task.prompt
        .clone()
        .or_else(|| {
            string_in_value(
                &task.raw_json,
                &[
                    "prompt",
                    "taskPrompt",
                    "task_prompt",
                    "description",
                    "summary",
                    "query",
                    "instructions",
                ],
            )
        })
        .filter(|prompt| !prompt.trim().is_empty())
}

fn task_result_message(result: &GrokTaskResult) -> Option<String> {
    result.message.clone().or_else(|| {
        string_in_value(
            &result.raw_json,
            &["summary", "message", "text", "content", "output", "result"],
        )
    })
}

fn task_result_timestamp(result: &GrokTaskResult) -> Option<String> {
    string_in_value(
        &result.raw_json,
        &[
            "createTime",
            "createdAt",
            "created_at",
            "completedAt",
            "completed_at",
            "lastRunAt",
            "last_run_at",
            "runAt",
            "run_at",
            "timestamp",
        ],
    )
}

fn latest_task_result_value(raw: &Value) -> Option<Value> {
    let dictionary = raw.as_object()?;
    for key in [
        "latestResult",
        "latest_result",
        "lastResult",
        "last_result",
        "taskResult",
        "task_result",
        "result",
    ] {
        if let Some(value) = dictionary.get(key) {
            return Some(value.clone());
        }
    }
    for key in ["results", "taskResults", "task_results", "runs"] {
        if let Some(value) = dictionary
            .get(key)
            .and_then(Value::as_array)
            .and_then(|values| values.last())
        {
            return Some(value.clone());
        }
    }
    None
}

fn compact_timestamp(value: String) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some((date, time_remainder)) = trimmed.split_once('T') {
        let hour_minute = time_remainder.chars().take(5).collect::<String>();
        if hour_minute.chars().count() == 5 {
            return format!("{date} {hour_minute}");
        }
    }
    trimmed.to_string()
}

fn task_run_timestamp_label(value: &str) -> String {
    task_run_timestamp_label_at(value, Local::now().fixed_offset()).unwrap_or_default()
}

fn task_run_timestamp_label_at(value: &str, now: DateTime<FixedOffset>) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let Some(date) = parse_task_run_date(value, now.offset()) else {
        return Some(value.to_string());
    };
    let exact = date.format("%Y-%m-%d %H:%M").to_string();
    let date_day = date.date_naive();
    let now_day = now.date_naive();
    let friendly = if date_day == now_day {
        "Today".to_string()
    } else if date_day == now_day - ChronoDuration::days(1) {
        "Yesterday".to_string()
    } else {
        let days = now_day.signed_duration_since(date_day).num_days();
        if days > 1 && days <= 7 {
            "Last week".to_string()
        } else if date.year() == now.year() {
            date.format("%b %-d").to_string()
        } else {
            date.format("%b %-d, %Y").to_string()
        }
    };
    Some(format!("{friendly} ({exact})"))
}

fn parse_task_run_date(value: &str, offset: &FixedOffset) -> Option<DateTime<FixedOffset>> {
    if let Ok(date) = DateTime::parse_from_rfc3339(value) {
        return Some(date.with_timezone(offset));
    }
    for format in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(date) = NaiveDateTime::parse_from_str(value, format) {
            return Some(
                DateTime::<Utc>::from_naive_utc_and_offset(date, Utc).with_timezone(offset),
            );
        }
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|date| DateTime::<Utc>::from_naive_utc_and_offset(date, Utc).with_timezone(offset))
}

fn run_ordinal_label(index: usize) -> String {
    match index {
        0 => "latest".to_string(),
        1 => "previous".to_string(),
        _ => format!("{} runs back", index + 1),
    }
}

fn string_in_value(value: &Value, keys: &[&str]) -> Option<String> {
    let dictionary = value.as_object()?;
    keys.iter().find_map(|key| {
        dictionary
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn bool_in_value(value: &Value, keys: &[&str]) -> Option<bool> {
    let dictionary = value.as_object()?;
    keys.iter()
        .find_map(|key| dictionary.get(*key).and_then(bool_value))
}

fn bool_value(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(value) => Some(*value),
        Value::Number(value) => value.as_f64().map(|number| number != 0.0),
        Value::String(value) => match value.trim().to_lowercase().as_str() {
            "true" | "yes" | "1" | "enabled" | "active" => Some(true),
            "false" | "no" | "0" | "disabled" | "inactive" | "archived" | "paused" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn agent_rows(agents: &[GrokAgentCustomization]) -> String {
    if agents.is_empty() {
        return "No agent customizations found.".to_string();
    }

    let mut lines = vec!["Agents:".to_string()];
    for agent in agents {
        lines.push(agent_summary(agent));
    }
    lines.join("\n")
}

fn agent_detail(agent: &GrokAgentCustomization) -> String {
    let mut lines = vec![
        format!("Agent {}: {}", agent.agent_id, agent.name),
        format!(
            "Instructions: {}",
            instruction_length_label(&agent.instructions)
        ),
    ];
    if !agent.instructions.is_empty() {
        lines.push(String::new());
        lines.push(agent.instructions.clone());
    }
    lines.join("\n")
}

fn agent_summary(agent: &GrokAgentCustomization) -> String {
    format!(
        "ID: {} | Name: {} | Instructions: {}",
        agent.agent_id,
        agent.name,
        instruction_length_label(&agent.instructions)
    )
}

fn instruction_length_label(instructions: &str) -> String {
    let count = instructions.chars().count();
    if count == 0 {
        "empty".to_string()
    } else if count == 1 {
        "1 char".to_string()
    } else {
        format!("{count} chars")
    }
}

fn workspace_summary(workspace: &GrokWorkspace) -> String {
    let id = workspace.resolved_id().unwrap_or("");
    let name = workspace.display_name().unwrap_or("");
    let model = workspace.preferred_model.as_deref().unwrap_or("");
    let icon = workspace.icon.as_deref().unwrap_or("");
    [
        format!("ID: {id}"),
        format!("Name: {name}"),
        format!("Model: {model}"),
        format!("Icon: {icon}"),
    ]
    .join("\n")
}

fn conversation_v2_rows(response: &GrokConversationV2Response, fallback_id: &str) -> String {
    let data = conversation_v2_json(response, fallback_id);
    let Some(data) = data.as_object() else {
        return format!("Conversation: {fallback_id}");
    };

    let mut lines = vec![format!(
        "Conversation: {}",
        data.get("conversationId")
            .and_then(Value::as_str)
            .unwrap_or(fallback_id)
    )];
    if let Some(title) = data.get("title").and_then(Value::as_str) {
        lines.push(format!("Title: {title}"));
    }
    if let Some(workspace_count) = data.get("workspaceCount").and_then(Value::as_u64) {
        lines.push(format!("Workspaces: {workspace_count}"));
    }
    if data
        .get("hasTaskResult")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        lines.push("TaskResult: yes".to_string());
    }
    lines.join("\n")
}

fn list_usage() -> String {
    [
        "Usage:",
        "  grok list [--json|--format json] [--debug]",
        "  grok list --conversation <conversationId> [--json|--format json] [--debug]",
        "  grok list delete <conversationId> --yes [--json|--format json] [--debug]",
    ]
    .join("\n")
}

fn chat_usage() -> String {
    [
        "Usage: grok chat [options] [initial message...]",
        "",
        "Starts interactive chat mode. If an initial message is supplied, Grok sends it first and then keeps the prompt open.",
        "",
        "Options:",
        "  --markdown, -m              Use markdown formatting in output",
        "  --raw                       Show raw Markdown text in output",
        "  --format <md|raw>           Choose output format",
        "  --debug                     Show debug information",
        "  --private                   Do not save the conversation",
        "  --stream                    Stream responses",
        "  --quiet                     Suppress UI/status output for piped raw scripting",
        "  --audio <path|->            Transcribe audio and send the transcript as the initial message",
        "  --audio-format <format>     Required with --audio - or unknown file extensions",
        "  --refinement-level <level>  Speech-to-text refinement level",
        "  --model, --mode <mode>      Use auto, fast, expert, grok-4.3-beta, heavy, or a raw modeId",
        "",
        "Reasoning is always enabled for all models. --reasoning is accepted as a legacy no-op deprecated on 2025-07-09, the Grok 4 release date.",
        "With --json, chat sends the initial message as one JSON result and exits.",
        "With --raw --quiet, piped chat writes assistant answers to stdout without prompts.",
    ]
    .join("\n")
}

fn message_usage() -> String {
    [
        "Usage: grok message [options] [message...]",
        "",
        "Sends one message to Grok and exits.",
        "",
        "Options:",
        "  --markdown, -m              Use markdown formatting in output",
        "  --raw                       Show raw Markdown text in output",
        "  --json                      Emit scriptable JSON output",
        "  --format <md|raw|json>      Choose output format",
        "  --debug                     Show debug information",
        "  --private                   Do not save the conversation",
        "  --stream                    Stream responses",
        "  --quiet                     Suppress UI/status output; useful with --raw in scripts",
        "  --stdin                     Read the message from stdin",
        "  --prompt-file <path>        Read the message from a UTF-8 text file",
        "  --file, --upload <path>     Upload and attach a local file before sending",
        "  --attach <fileId>           Attach an existing Grok file ID before sending",
        "  --audio <path|->            Transcribe audio and send the transcript as the message",
        "  --audio-format <format>     Required with --audio - or unknown file extensions",
        "  --refinement-level <level>  Speech-to-text refinement level",
        "  --model, --mode <mode>      Use auto, fast, expert, grok-4.3-beta, heavy, or a raw modeId",
        "",
        "Message input:",
        "  - message args, --audio, --prompt-file, and --stdin are mutually exclusive",
        "  - if no message args or prompt file are supplied and stdin is piped, stdin is used",
        "",
        "Reasoning is always enabled for all models. --reasoning is accepted as a legacy no-op deprecated on 2025-07-09, the Grok 4 release date.",
        "JSON mode writes only JSON to stdout. Human progress and debug banners are suppressed.",
        "With --stream --json, output is NDJSON: one JSON event object per line.",
        "With --raw --quiet, stdout contains only assistant answer text.",
    ]
    .join("\n")
}

fn models_usage() -> String {
    let mut lines = vec![
        "Usage: grok models [--json|--format json]".to_string(),
        String::new(),
        "Shows available Grok web modes. With JSON output, stdout contains one JSON result object."
            .to_string(),
        String::new(),
    ];
    lines.push(available_models_text(
        Some(&GrokMode::default_mode()),
        &GrokMode::known_modes(),
    ));
    lines.join("\n")
}

fn help_text() -> String {
    let command_width = visible_interactive_commands()
        .iter()
        .map(|command| command.len())
        .max()
        .unwrap_or(20)
        .max(20);
    let chat_commands = visible_interactive_commands()
        .iter()
        .map(|command| {
            format!(
                "          {command:command_width$} - {}",
                interactive_command_description(command)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"Grok 4 up in your terminal

Usage: grok [command] [options]

Running just 'grok' with no commands starts an interactive chat session.

Commands:
  chat              - Start an interactive chat session
  message <text>    - Send a message to Grok and exit
  transcribe <file>  - Transcribe audio and print the text
  auth              - Authentication commands
  list              - List and manage saved conversations
  models            - Show available Grok web modes
  agents            - Manage Grok agent settings
  tasks             - List, create, and archive Grok tasks
  skills            - List Grok skills and your enabled skills
  workspaces        - List, create, inspect, and delete Grok workspaces
  files             - Upload, list, and delete Grok assets
  help              - Show help information

App Options:
  --markdown, -m    - Use markdown formatting in output (default)
  --raw             - Show raw Markdown text in output
  --json            - Emit scriptable JSON output
  --format <md|raw|json> - Choose output format
  --debug           - Show debug information
  --private         - Enable private mode (conversations will not be saved)
  --stream          - Stream responses as they are generated
  --quiet           - Suppress UI/status output for scriptable raw text
  --file <path>     - Upload and attach a local file before sending a message
  --attach <fileId> - Attach an existing Grok file ID before sending
  --audio <path|->  - Transcribe audio before sending a message
  --audio-format <format> - Required with audio stdin or unknown extensions
  --refinement-level <level> - Speech-to-text refinement level
  --model <mode>    - Use auto, fast, expert, grok-4.3-beta, heavy, or a raw modeId

JSON Mode:
  - Every CLI command accepts --json, --format json, or --format=json
  - stdout is reserved for JSON; human progress and debug banners are suppressed
  - Use an initial message for JSON chat output
  - grok message --stream --json emits NDJSON, one event object per line

Scriptable Text:
  - grok message --raw --quiet writes only assistant answer text to stdout
  - grok message reads piped stdin when no message args or prompt file are supplied
  - grok message --prompt-file <path> reads a UTF-8 prompt file
  - grok message --file <path> uploads, attaches, and sends one prompt
  - grok message --audio <path> transcribes audio, then sends the transcript
  - grok transcribe <path> prints only the transcript
  - grok chat --raw --quiet supports cleaner piped multi-message sessions

Chat Commands:
{chat_commands}

Notes:
  - In chat mode, conversation context is maintained between messages
  - Use '/new' to start a new conversation thread
  - Use 'exit' or '/exit' to exit the app
  - The message command always starts a new conversation without context
  - Reasoning is always enabled for all models; --reasoning and /reason are legacy no-ops deprecated on 2025-07-09, the Grok 4 release date

Examples:
  grok                                      - Start interactive chat mode
  grok Hello                                - Start chat with initial message "Hello"
  grok message Hello, how are you today?    - Send a message and exit
  grok message --model expert Explain this  - Send a message using Expert
  cat prompt.md | grok message --raw --quiet - Send piped prompt text
  grok message --raw --quiet --prompt-file prompt.md
  grok message --file paper.pdf "What matters here?"
  grok message --json Explain this briefly  - Send a message and print JSON
  grok message --stream --json Draft a note  - Stream NDJSON events
  grok message --audio note.webm --raw --quiet - Send an audio transcript
  grok transcribe note.webm                  - Print an audio transcript
  grok auth                                 - Generate new credentials from browser cookies
  grok auth generate --json                  - Generate credentials and print JSON
  grok auth import /path/to/credentials.json --json
  grok models                               - Show available web modes
  grok models --json                        - Show available web modes as JSON
  grok agents list                          - List built-in agent IDs
  grok agents show 0                        - Show full agent instructions
  grok agents edit 0                        - Edit agent instructions in $EDITOR
  grok agents set 0 --instructions "Answer briefly."
  grok tasks                                - List tasks
  grok tasks list --json                    - List tasks as JSON
  grok tasks inactive                       - List archived tasks
  grok tasks show <taskId>                  - Show task details and latest result
  grok tasks results <taskId> --limit 10    - Show recent task runs
  grok tasks chat <taskId> --run previous --message "Explain this"
  grok skills                               - List skills
  grok workspaces                           - List workspaces
  grok workspaces delete <workspaceId>      - Delete a workspace
  grok files list                           - List recent assets
  grok files delete <fileId>                - Delete an asset
  grok list                                 - List and select from saved conversations
  grok list --json                          - List conversations as JSON
  grok list delete <conversationId> --yes --json
"#
    )
}

fn help_json() -> Result<String> {
    let result = CliJsonResult::ok(
        "help",
        None,
        "help",
        json!({
            "usage": "grok [command] [options]",
            "commands": router::recognized_top_level_commands().into_iter().collect::<Vec<_>>(),
            "interactiveCommands": visible_interactive_commands(),
            "jsonOptions": ["--json", "--format json", "--format=json"],
            "streaming": "grok message --stream --json emits NDJSON"
        }),
        default_json_meta(false, Vec::new()),
    );
    Ok(serde_json::to_string_pretty(&result)?)
}

fn visible_interactive_commands() -> Vec<&'static str> {
    visible_interactive_command_specs()
        .into_iter()
        .map(|spec| spec.usage)
        .collect()
}

fn interactive_command_description(command: &str) -> &'static str {
    visible_interactive_command_specs()
        .into_iter()
        .find(|spec| spec.usage == command)
        .map(|spec| spec.description)
        .unwrap_or("")
}

fn disabled_code_message() -> String {
    "Grok Code is disabled: the code harness concept will not work with grok.com because tool calls happen server side.".to_string()
}

fn interactive_help() -> String {
    let visible_commands = visible_interactive_command_specs();
    let mut lines = vec![
        String::new(),
        "Basic Commands:".to_string(),
        "- new: Start a new conversation thread".to_string(),
        "- help: Show this help message".to_string(),
        "- exit: Exit the app".to_string(),
        String::new(),
        "Slash Commands:".to_string(),
    ];

    for category in InteractiveCommandCategory::ALL {
        let commands = visible_commands
            .iter()
            .copied()
            .filter(|spec| spec.category == *category)
            .collect::<Vec<_>>();
        if commands.is_empty() {
            continue;
        }
        lines.push(String::new());
        lines.push(format!("{}:", category.title()));
        for spec in commands {
            lines.push(format!("- {}: {}", spec.usage, spec.description));
        }
    }

    lines.extend([
        String::new(),
        "Modes:".to_string(),
        "- Model: auto, fast, expert, grok-4.3-beta, heavy, or a raw modeId".to_string(),
        "- Private Mode: When enabled, conversations will not be saved".to_string(),
        "- Streaming: Displays responses as they are generated".to_string(),
        "- Output Format: Markdown is default; Raw preserves source Markdown".to_string(),
        "- Agents: Configure instructions in Grok agent settings".to_string(),
        String::new(),
        "Note: Reasoning is always enabled for all models; /reason is a legacy no-op deprecated on 2025-07-09, the Grok 4 release date.".to_string(),
        String::new(),
    ]);
    lines.join("\n")
}

fn list_delete_usage() -> String {
    "Usage: grok list delete <conversationId> --yes [--json|--format json] [--debug]".to_string()
}

fn files_usage() -> String {
    [
        "Files:",
        "  grok files upload <path> [--mime <mime>] [--json|--format json]",
        "  grok files list [--json|--format json] [--page-size N]",
        "  grok files delete <fileId> [--json|--format json]",
    ]
    .join("\n")
}

fn files_list_usage() -> String {
    "grok files list [--json|--format json] [--page-size N]".to_string()
}

fn files_upload_usage() -> String {
    "grok files upload <path> [--mime <mime>] [--json|--format json]".to_string()
}

fn files_delete_usage() -> String {
    "grok files delete <fileId> [--json|--format json]".to_string()
}

fn transcribe_usage() -> String {
    "grok transcribe [--audio-format <format>] [--refinement-level <level>] <path|->".to_string()
}

fn transcribe_help_text() -> String {
    [
        "Usage: grok transcribe [options] <path|->",
        "",
        "Transcribes an audio file and prints only the transcript by default.",
        "",
        "Options:",
        "  --raw                       Print transcript text (default)",
        "  --json                      Emit scriptable JSON output",
        "  --format <raw|json>         Choose output format",
        "  --audio-format <format>     Required with - or unknown file extensions",
        "  --refinement-level <level>  Speech-to-text refinement level",
        "  --debug                     Show debug information",
        "  --quiet                     Suppress status output",
        "",
        "Examples:",
        "  grok transcribe note.webm",
        "  grok transcribe --audio-format webm -",
        "  grok transcribe --json meeting.m4a",
    ]
    .join("\n")
}

fn file_name_for_upload(path: &Path) -> Result<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Upload path has no file name: {}", path.display()))
}

fn inferred_file_mime_type(path: &Path) -> String {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_lowercase)
        .as_deref()
    {
        Some("txt") => "text/plain",
        Some("json") => "application/json",
        Some("csv") => "text/csv",
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("pptx") => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        Some("md") => "text/markdown",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn expand_tilde_path(path: PathBuf) -> PathBuf {
    let Some(value) = path.to_str() else {
        return path;
    };
    if value == "~" {
        return dirs::home_dir().unwrap_or(path);
    }
    if let Some(rest) = value.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    path
}

fn workspaces_usage() -> String {
    [
        "Workspaces:",
        "  grok workspaces list [--json|--format json]",
        "  grok workspaces create --name <name> [--icon <icon>] [--personality <text>] [--model <mode>] [--json|--format json]",
        "  grok workspaces add-conversation <workspaceId> <conversationId> [--json|--format json]",
        "  grok workspaces delete <workspaceId> [--json|--format json]",
        "  grok workspaces conversation <conversationId> [--json|--format json]",
    ]
    .join("\n")
}

fn skills_usage() -> String {
    [
        "Skills:",
        "  grok skills list [--json|--format json]",
        "  grok skills mine [--json|--format json]",
        "  grok skills user [--json|--format json]",
    ]
    .join("\n")
}

fn skills_list_usage() -> String {
    "Usage: grok skills list [--json|--format json] [--debug]".to_string()
}

fn skills_mine_usage() -> String {
    "Usage: grok skills mine [--json|--format json] [--debug]".to_string()
}

fn skills_user_usage() -> String {
    "Usage: grok skills user [--json|--format json] [--debug]".to_string()
}

fn tasks_usage() -> String {
    [
        "Tasks:",
        "  grok tasks list [--json|--format json]",
        "  grok tasks inactive [--json|--format json]",
        "  grok tasks select",
        "  grok tasks show <taskId> [--json|--format json]",
        "  grok tasks results <taskId> [--limit N] [--json|--format json]",
        "  grok tasks chat <taskId|taskName> [--run latest|previous|N|RESULT_ID] [--limit N] [--message <text>] [--json|--format json]",
        "  grok tasks create --prompt <text> [--name <name>] [--date YYYY-MM-DD] [--time HH:mm] [--timezone TZ] [--guideline <text>] [--model-mode BASE] [--json|--format json]",
        "  grok tasks archive <taskId> [--json|--format json]",
    ]
    .join("\n")
}

fn tasks_create_usage() -> String {
    "Usage: grok tasks create --prompt <text> [--name <name>] [--date YYYY-MM-DD] [--time HH:mm] [--timezone TZ] [--guideline <text>] [--model-mode BASE] [--json|--format json]".to_string()
}

fn tasks_list_usage() -> String {
    "Usage: grok tasks list [--inactive|--archived] [--json|--format json] [--debug]".to_string()
}

fn tasks_inactive_usage() -> String {
    "Usage: grok tasks inactive [--json|--format json] [--debug]".to_string()
}

fn tasks_select_usage() -> String {
    "Usage: grok tasks select".to_string()
}

fn tasks_show_usage() -> String {
    "Usage: grok tasks show <taskId> [--json|--format json] [--debug]".to_string()
}

fn tasks_results_usage() -> String {
    "Usage: grok tasks results <taskId> [--limit N] [--json|--format json] [--debug]".to_string()
}

fn tasks_chat_usage() -> String {
    "Usage: grok tasks chat <taskId|taskName> [--run latest|previous|N|RESULT_ID] [--limit N] [--message <text>] [--model MODEL] [--json|--format json] [--debug]".to_string()
}

fn tasks_archive_usage() -> String {
    "Usage: grok tasks archive <taskId> [--json|--format json] [--debug]".to_string()
}

fn agents_usage() -> String {
    [
        "Agents:",
        "  grok agents list [--include-instructions] [--json|--format json]",
        "  grok agents show <agentId> [--json|--format json]",
        "  grok agents edit <agentId> [--replace] [--include-instructions] [--json|--format json]",
        "  grok agents set <agentId> --instructions <text> [--name <name>] [--replace] [--include-instructions] [--json|--format json]",
        "  grok agents set <agentId> --file <path> [--name <name>] [--replace] [--include-instructions] [--json|--format json]",
        "  grok agents clear <agentId> [--replace] [--include-instructions] [--json|--format json]",
        "",
        "Agent availability depends on the account. Basic accounts usually expose agent 0; SuperGrok accounts can expose agents 0 through 3.",
        "JSON list and mutation responses redact instruction text by default; pass --include-instructions to include it.",
        "The edit command opens $VISUAL, $EDITOR, or vi with the current instructions.",
    ]
    .join("\n")
}

fn agents_list_usage() -> String {
    "Usage: grok agents list [--include-instructions] [--json|--format json] [--debug]".to_string()
}

fn agents_show_usage() -> String {
    "Usage: grok agents show <agentId> [--json|--format json] [--debug]".to_string()
}

fn agents_set_usage() -> String {
    "Usage: grok agents set <agentId> (--instructions <text>|--file <path>) [--name <name>] [--replace] [--include-instructions] [--json|--format json] [--debug]".to_string()
}

fn agents_edit_usage() -> String {
    "Usage: grok agents edit <agentId> [--replace] [--include-instructions] [--json|--format json] [--debug]".to_string()
}

fn agents_clear_usage() -> String {
    "Usage: grok agents clear <agentId> [--replace] [--include-instructions] [--json|--format json] [--debug]".to_string()
}

fn workspaces_create_usage() -> String {
    "Usage: grok workspaces create --name <name> [--icon <icon>] [--personality <text>] [--model <mode>] [--json|--format json]".to_string()
}

fn workspaces_list_usage() -> String {
    "Usage: grok workspaces list [--json|--format json] [--debug]".to_string()
}

fn workspaces_add_conversation_usage() -> String {
    "Usage: grok workspaces add-conversation <workspaceId> <conversationId> [--json|--format json] [--debug]".to_string()
}

fn workspaces_delete_usage() -> String {
    "Usage: grok workspaces delete <workspaceId> [--json|--format json] [--debug]".to_string()
}

fn workspaces_conversation_usage() -> String {
    "Usage: grok workspaces conversation <conversationId> [--json|--format json] [--debug]"
        .to_string()
}

async fn run_auth_command(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
) -> Result<String> {
    let json_requested = top_level_options.json
        || top_level_options
            .format
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("json"))
        || is_json_requested(&args);
    let quiet_requested = args.iter().any(|arg| arg == "--quiet");
    let args = remove_json_output_args(normalized_auth_args(args));

    if args
        .first()
        .is_some_and(|arg| router::is_help_argument(arg))
    {
        return Ok(auth_usage());
    }

    let subcommand = args.first().map(String::as_str).unwrap_or("generate");
    match subcommand {
        "import" => {
            let import_args = args[1..]
                .iter()
                .filter(|arg| arg.as_str() != "--quiet")
                .cloned()
                .collect::<Vec<_>>();
            run_auth_import(&import_args, json_requested, quiet_requested)
        }
        "help" => Ok(auth_usage()),
        "generate" => run_auth_generate(&args[1..], json_requested, quiet_requested),
        unknown => {
            if unknown.starts_with('-') {
                run_auth_generate(&args, json_requested, quiet_requested)
            } else if json_requested {
                let result = CliJsonResult::error(
                    "auth",
                    None,
                    format!("Unknown auth command: {unknown}"),
                    "usage_error",
                    2,
                    false,
                    None,
                );
                Ok(serde_json::to_string_pretty(&result)?)
            } else {
                Ok(format!(
                    "Unknown auth command: {unknown}\nRun 'grok auth help' for available auth commands."
                ))
            }
        }
    }
}

fn run_auth_generate(
    args: &[String],
    json_requested: bool,
    quiet_requested: bool,
) -> Result<String> {
    if args.iter().any(|arg| router::is_help_argument(arg)) {
        return Ok(auth_generate_usage());
    }

    let mut extractor_args = args.to_vec();
    if json_requested && !extractor_args.iter().any(|arg| arg == "--quiet") {
        extractor_args.push("--quiet".to_string());
    }

    let mut human_lines = Vec::new();
    if !json_requested && !quiet_requested {
        human_lines.push("Extracting credentials from browser...".to_string());
    }

    match config::run_cookie_extractor(&extractor_args, json_requested || quiet_requested) {
        Ok(credentials_path) if json_requested => {
            let mut data = Map::new();
            data.insert("action".to_string(), json!("generate"));
            data.insert(
                "credentialsPath".to_string(),
                json!(credentials_path.display().to_string()),
            );
            if let Some(browser) = auth_browser_value(&extractor_args) {
                data.insert("browser".to_string(), json!(browser));
            }
            let result = CliJsonResult::ok(
                "auth",
                Some("generate".to_string()),
                "auth_result",
                Value::Object(data),
                default_json_meta(false, Vec::new()),
            );
            Ok(serde_json::to_string_pretty(&result)?)
        }
        Ok(_) if quiet_requested => Ok(String::new()),
        Ok(credentials_path) => {
            human_lines.push("Successfully generated credentials!".to_string());
            human_lines.push(format!("Saved to: {}", credentials_path.display()));
            Ok(human_lines.join("\n"))
        }
        Err(error) if json_requested => {
            let result = CliJsonResult::error(
                "auth",
                Some("generate".to_string()),
                error.to_string(),
                "api_error",
                1,
                false,
                Some(error.to_string()),
            );
            Ok(serde_json::to_string_pretty(&result)?)
        }
        Err(error) if quiet_requested => Ok(format!("Error generating credentials: {error}")),
        Err(error) => {
            human_lines.push(format!("Error generating credentials: {error}"));
            human_lines
                .push("Please make sure you're logged in to Grok in your browser.".to_string());
            Ok(human_lines.join("\n"))
        }
    }
}

fn run_auth_import(args: &[String], json_requested: bool, quiet_requested: bool) -> Result<String> {
    if args.iter().any(|arg| router::is_help_argument(arg)) {
        return Ok(auth_import_usage());
    }

    if args.len() != 1 {
        if json_requested {
            let result = CliJsonResult::error(
                "auth",
                Some("import".to_string()),
                "Please provide a path to the credentials file",
                "usage_error",
                2,
                false,
                None,
            );
            return Ok(serde_json::to_string_pretty(&result)?);
        }
        return Ok("Error: Please provide a path to the credentials file".to_string());
    }

    let path = &args[0];
    let mut human_lines = Vec::new();
    if !json_requested && !quiet_requested {
        human_lines.push(format!("Importing credentials from {path}..."));
    }

    match config::save_credentials_path(path) {
        Ok(_saved_path) if json_requested => {
            let result = CliJsonResult::ok(
                "auth",
                Some("import".to_string()),
                "auth_result",
                json!({
                    "action": "import",
                    "path": path
                }),
                default_json_meta(false, Vec::new()),
            );
            Ok(serde_json::to_string_pretty(&result)?)
        }
        Ok(_) if quiet_requested => Ok(String::new()),
        Ok(_) => {
            human_lines.push("Successfully imported credentials!".to_string());
            Ok(human_lines.join("\n"))
        }
        Err(error) if json_requested => {
            let result = CliJsonResult::error(
                "auth",
                Some("import".to_string()),
                error.to_string(),
                "api_error",
                1,
                false,
                Some(error.to_string()),
            );
            Ok(serde_json::to_string_pretty(&result)?)
        }
        Err(error) if quiet_requested => Ok(format!("Error importing credentials: {error}")),
        Err(error) => {
            human_lines.push(format!("Error importing credentials: {error}"));
            human_lines.push(
                "Please make sure the file exists and contains valid credentials.".to_string(),
            );
            Ok(human_lines.join("\n"))
        }
    }
}

fn normalized_auth_args(args: Vec<String>) -> Vec<String> {
    let Some(first) = args.first() else {
        return args;
    };
    let browser = first.to_lowercase();
    if matches!(
        browser.as_str(),
        "auto" | "safari" | "atlas" | "chrome" | "firefox" | "chromium" | "brave" | "edge" | "arc"
    ) {
        let mut normalized = vec!["generate".to_string(), "--browser".to_string(), browser];
        normalized.extend(args.into_iter().skip(1));
        normalized
    } else {
        args
    }
}

fn remove_json_output_args(args: Vec<String>) -> Vec<String> {
    let mut result = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--json" {
            index += 1;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--format=")
            && value.eq_ignore_ascii_case("json")
        {
            index += 1;
            continue;
        }
        if arg == "--format"
            && index + 1 < args.len()
            && args[index + 1].eq_ignore_ascii_case("json")
        {
            index += 2;
            continue;
        }
        result.push(arg.clone());
        index += 1;
    }
    result
}

fn auth_browser_value(args: &[String]) -> Option<String> {
    for (index, arg) in args.iter().enumerate() {
        if arg == "--browser" {
            return args.get(index + 1).cloned();
        }
        if let Some(value) = arg.strip_prefix("--browser=") {
            return Some(value.to_string());
        }
    }
    None
}

fn auth_usage() -> String {
    [
        "Auth commands:",
        "  auth          - Generate new credentials from browser cookies",
        "  generate      - Generate new credentials from browser cookies",
        "  safari        - Generate credentials from Safari",
        "  chrome        - Generate credentials from Chrome",
        "  <browser>     - Browser shortcut: auto, safari, atlas, chrome, firefox, chromium, brave, edge, arc",
        "  import <file> - Import credentials from a JSON file",
        "  Add --json, --format json, or --format=json for JSON output",
    ]
    .join("\n")
}

fn auth_generate_usage() -> String {
    [
        "Usage: grok auth generate [--browser <name>] [--quiet] [--json|--format json]",
        "",
        "Generates credentials from browser cookies.",
        "Browser shortcuts: auto, safari, atlas, chrome, firefox, chromium, brave, edge, arc",
    ]
    .join("\n")
}

fn auth_import_usage() -> String {
    [
        "Usage: grok auth import <file> [--json|--format json]",
        "",
        "Imports credentials from a JSON file.",
    ]
    .join("\n")
}

fn run_test_command(
    args: Vec<String>,
    top_level_options: &options::GrokCommandOptions,
) -> Result<String> {
    if args.len() == 1 && router::is_help_argument(&args[0]) {
        return Ok(test_usage());
    }

    let json_requested = top_level_options.json || is_json_requested(&args);
    let words = remove_json_output_args(args);
    let message = words.join(" ");
    if json_requested {
        let result = CliJsonResult::ok(
            "test",
            None,
            "test_result",
            json!({
                "provided": !words.is_empty(),
                "message": message
            }),
            default_json_meta(false, top_level_options.warnings()),
        );
        return Ok(serde_json::to_string_pretty(&result)?);
    }

    let mut lines = vec!["Test command executed successfully!".to_string()];
    if message.is_empty() {
        lines.push("No message provided.".to_string());
    } else {
        lines.push(format!("Message provided: \"{message}\""));
    }
    Ok(lines.join("\n"))
}

fn test_usage() -> String {
    "Usage: grok test [message...]".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_matcher_scores_subsequence_matches_like_swift() {
        assert!(fuzzy::score("exp", "Expert expert").is_some());
        assert!(fuzzy::score("wrk", "workspace Research Notes").is_some());
        assert_eq!(fuzzy::score("zzz", "workspace Research Notes"), None);
        assert!(fuzzy::score("/workspace", "/workspace") > fuzzy::score("/wrk", "/workspace"));
    }

    #[test]
    fn interactive_command_registry_suggests_swift_typo_hints() {
        assert_eq!(
            nearest_interactive_command("wrkspace").map(|spec| spec.command),
            Some("/workspace")
        );
        assert_eq!(
            nearest_interactive_command("workspaces").map(|spec| spec.command),
            Some("/workspace")
        );
        assert_eq!(
            nearest_interactive_command("list").map(|spec| spec.command),
            Some("/resume")
        );
        assert_eq!(nearest_interactive_command("wat"), None);
    }

    #[test]
    fn slash_completion_prioritizes_model_and_hides_advanced_commands_like_swift() {
        let bare = interactive_completion_suggestion_displays("/", &[]);

        assert_eq!(bare.first().map(String::as_str), Some("/model"));
        assert!(bare.contains(&"/limits".to_string()));
        assert!(bare.contains(&"/search".to_string()));
        assert!(!bare.contains(&"/stream".to_string()));
        assert!(!bare.contains(&"/special".to_string()));

        assert!(
            interactive_completion_suggestion_displays("/sea", &[])
                .contains(&"/search".to_string())
        );
        assert!(
            interactive_completion_suggestion_displays("/str", &[])
                .contains(&"/stream".to_string())
        );
        assert!(
            !interactive_completion_suggestion_displays("/spe", &[])
                .contains(&"/special".to_string())
        );
    }

    #[test]
    fn slash_completion_uses_singular_workspace_command_like_swift() {
        let suggestions = interactive_completion_suggestion_displays("/", &[]);

        assert!(suggestions.contains(&"/workspace".to_string()));
        assert!(!suggestions.contains(&"/workspaces".to_string()));
        assert!(suggestions.contains(&"/resume".to_string()));
        assert!(!suggestions.contains(&"/list".to_string()));
        assert!(suggestions.contains(&"/goal".to_string()));
        assert!(!suggestions.contains(&"/reset-conversation".to_string()));

        assert!(
            interactive_completion_suggestion_displays("/skill c", &[])
                .contains(&"/skill create".to_string())
        );
        let skill_create = interactive_completion_suggestions("/skill c", &[])
            .into_iter()
            .find(|suggestion| suggestion.display == "/skill create")
            .map(|suggestion| (suggestion.insert_text, suggestion.requires_argument));
        assert_eq!(skill_create, Some(("/skill create".to_string(), true)));
        assert_eq!(
            interactive_completion_suggestion_displays("/workspaces", &[]),
            vec!["/workspace".to_string()]
        );
    }

    #[test]
    fn remote_completion_suggestions_are_used_for_free_text_only_like_swift() {
        let remote_suggestions = vec![
            InputTypeaheadSuggestion::new("test driven development"),
            InputTypeaheadSuggestion::new("testing rust"),
        ];

        assert_eq!(
            interactive_completion_suggestion_displays("test", &remote_suggestions),
            vec![
                "test driven development".to_string(),
                "testing rust".to_string()
            ]
        );
        assert!(
            !interactive_completion_suggestion_displays("/", &remote_suggestions)
                .contains(&"test driven development".to_string())
        );
    }

    #[test]
    fn unknown_interactive_command_includes_nearest_suggestion_like_swift() {
        assert_eq!(
            unknown_interactive_command_lines("wrkspace"),
            vec![
                "Unknown command /wrkspace".to_string(),
                "Did you mean /workspace".to_string()
            ]
        );
        assert_eq!(
            unknown_interactive_command_lines("wat"),
            vec!["Unknown command /wat".to_string()]
        );
    }

    #[test]
    fn json_error_details_match_swift_error_code_mapping() {
        let unauthorized =
            json_error_details(&anyhow::Error::new(GrokError::Unauthorized), "api_error");
        assert_eq!(unauthorized.code, "auth_error");
        assert!(unauthorized.recoverable);
        assert_eq!(
            unauthorized.message,
            "Grok rejected the saved cookies. Re-run `grok auth generate` after logging in."
        );

        let access_denied = json_error_details(
            &anyhow::Error::new(GrokError::AccessDenied(
                "Grok denied access to Heavy.".to_string(),
            )),
            "api_error",
        );
        assert_eq!(access_denied.code, "access_denied");
        assert!(!access_denied.recoverable);

        let decoding = json_error_details(
            &anyhow::Error::new(GrokError::Decoding("bad json".to_string())),
            "api_error",
        );
        assert_eq!(decoding.code, "decoding_error");

        let usage = json_error_details(
            &anyhow::anyhow!("Usage: grok message [options]"),
            "usage_error",
        );
        assert_eq!(usage.code, "usage_error");
        assert_eq!(usage.message, "Usage: grok message [options]");
    }

    #[test]
    fn json_error_details_rewrite_rate_limit_messages_like_swift() {
        let rate_limited = json_error_details(
            &anyhow::Error::new(GrokError::Api(
                "HTTP Error: 429: too many requests, wait 1 minute".to_string(),
            )),
            "api_error",
        );

        assert_eq!(rate_limited.code, "rate_limit");
        assert_eq!(
            rate_limited.message,
            "Message limit reached. Grok is rate limiting this account right now. Wait 1 minute, then try again."
        );
        assert_eq!(
            rate_limited.raw_message,
            "HTTP Error: 429: too many requests, wait 1 minute"
        );

        let fallback = json_error_details(
            &anyhow::Error::new(GrokError::Api(
                "HTTP Error: 429: too many requests".to_string(),
            )),
            "api_error",
        );
        assert_eq!(fallback.code, "rate_limit");
        assert!(fallback.message.contains("Wait a few minutes"));
    }

    #[test]
    fn interactive_help_uses_registry_grouped_output_like_swift() {
        let help = interactive_help();

        assert!(help.contains("Basic Commands:"));
        assert!(help.contains("Slash Commands:"));
        assert!(help.contains("Session:"));
        assert!(help.contains("- /goal [objective|pause|resume|clear|complete]: Run a durable objective until completion"));
        assert!(help.contains("Model:"));
        assert!(help.contains("- /format [md|raw]: Toggle Markdown/Raw output"));
        assert!(help.contains("Files:"));
        assert!(help.contains("Workspace:"));
        assert!(help.contains("Library:"));
        assert!(help.contains("- /agents [list|show|edit|set]: Manage agent settings"));
        assert!(help.contains("Auth:"));
        assert!(help.contains("Audio:"));
        assert!(help.contains("Utility:"));
        assert!(help.contains("Modes:"));
        assert!(!help.contains("Agent Commands:"));
        assert!(!help.contains("sync-custom"));
    }

    #[test]
    fn avfoundation_default_device_specifier_rejects_names_ffmpeg_parses_as_indexes_like_swift() {
        assert_eq!(
            avfoundation_specifier_for_default_audio_device_name("MacBook Pro Microphone"),
            Some("MacBook Pro Microphone".to_string())
        );
        assert_eq!(
            avfoundation_specifier_for_default_audio_device_name("009 AirMax"),
            None
        );
        assert_eq!(
            avfoundation_specifier_for_default_audio_device_name("1MORE Headset"),
            None
        );
        assert_eq!(
            avfoundation_specifier_for_default_audio_device_name("Built-in: Microphone"),
            None
        );
        assert_eq!(
            avfoundation_specifier_for_default_audio_device_name("   "),
            None
        );
    }

    #[test]
    fn avfoundation_audio_input_argument_uses_audio_only_specifier_like_swift() {
        assert_eq!(
            avfoundation_audio_input_argument("MacBook Pro Microphone"),
            ":MacBook Pro Microphone"
        );
        assert_eq!(avfoundation_audio_input_argument("1"), ":1");
        assert_eq!(avfoundation_audio_input_argument(":1"), ":1");
        assert_eq!(avfoundation_audio_input_argument("   "), ":default");
    }

    #[test]
    fn task_create_default_datetime_uses_requested_timezone_like_swift()
    -> Result<(), Box<dyn std::error::Error>> {
        let now = DateTime::parse_from_rfc3339("2026-05-15T23:30:00Z")?.with_timezone(&Utc);

        assert_eq!(
            task_datetime_strings_at(now, "Asia/Bangkok"),
            ("2026-05-16".to_string(), "06:30".to_string())
        );
        assert_eq!(
            task_datetime_strings_at(now, "America/New_York"),
            ("2026-05-15".to_string(), "19:30".to_string())
        );
        assert_eq!(
            task_datetime_strings_at(now, "not-a-timezone"),
            ("2026-05-16".to_string(), "06:30".to_string())
        );
        Ok(())
    }

    #[test]
    fn task_run_timestamp_label_formats_recent_dates_like_swift()
    -> Result<(), Box<dyn std::error::Error>> {
        let now = DateTime::parse_from_rfc3339("2026-05-14T12:00:00Z")?;

        assert_eq!(
            task_run_timestamp_label_at("2026-05-14T08:30:00Z", now),
            Some("Today (2026-05-14 08:30)".to_string())
        );
        assert_eq!(
            task_run_timestamp_label_at("2026-05-13T08:30:00Z", now),
            Some("Yesterday (2026-05-13 08:30)".to_string())
        );
        assert_eq!(
            task_run_timestamp_label_at("2026-05-07T08:30:00Z", now),
            Some("Last week (2026-05-07 08:30)".to_string())
        );
        assert_eq!(
            task_run_timestamp_label_at("2026-05-01T08:30:00Z", now),
            Some("May 1 (2026-05-01 08:30)".to_string())
        );
        Ok(())
    }

    #[test]
    fn task_run_timestamp_label_falls_back_to_raw_value() {
        assert_eq!(
            task_run_timestamp_label_at(
                "not-a-date",
                DateTime::parse_from_rfc3339("2026-05-14T12:00:00Z")
                    .unwrap_or_else(|_| Local::now().fixed_offset())
            ),
            Some("not-a-date".to_string())
        );
        assert_eq!(
            task_run_timestamp_label_at(
                "",
                DateTime::parse_from_rfc3339("2026-05-14T12:00:00Z")
                    .unwrap_or_else(|_| Local::now().fixed_offset())
            ),
            None
        );
    }
}
