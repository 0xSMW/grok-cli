use crate::options::OutputFormat;
use crate::terminal::{strip_ansi, truncate_end};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliHudState {
    pub model_name: String,
    pub workspace_name: Option<String>,
    pub private_mode: bool,
    pub stream: bool,
    pub output_format: OutputFormat,
    pub attached_file_count: usize,
    pub rate_limit_warning: Option<String>,
}

pub fn lines(state: &CliHudState, width: usize) -> Vec<String> {
    let label = state.workspace_name.as_deref().unwrap_or("Grok");
    let mut segments = Vec::new();

    if state.private_mode {
        segments.push("Private".to_string());
    }

    segments.push(model_status_name(&state.model_name, label));
    segments.push(state.output_format.status_name().to_string());

    if !state.stream {
        segments.push("Stream off".to_string());
    }

    if state.attached_file_count > 0 {
        let file_label = if state.attached_file_count == 1 {
            "1 file".to_string()
        } else {
            format!("{} files", state.attached_file_count)
        };
        segments.push(file_label);
    }

    let plain_status = truncate_end(&format!("{label} > {}", segments.join(" | ")), width);
    let mut lines = vec![color_status_line(&plain_status, label)];

    if let Some(warning) = state.rate_limit_warning.as_deref()
        && !warning.is_empty()
    {
        let plain_warning = truncate_end(&format!("Limit > {warning}"), width);
        lines.push(color_warning_line(&plain_warning));
    }

    lines
}

pub fn plain_lines(state: &CliHudState, width: usize) -> Vec<String> {
    lines(state, width)
        .into_iter()
        .map(|line| strip_ansi(&line))
        .collect()
}

fn color_status_line(line: &str, label: &str) -> String {
    let prefix = format!("{label} > ");
    let Some(rest) = line.strip_prefix(&prefix) else {
        return cyan(line);
    };

    format!("{}{}{}", cyan(label), cyan(" > "), yellow(rest))
}

fn color_warning_line(line: &str) -> String {
    let Some(rest) = line.strip_prefix("Limit > ") else {
        return yellow(line);
    };

    format!("{}{}{}", yellow("Limit"), cyan(" > "), yellow(rest))
}

fn model_status_name(model_name: &str, label: &str) -> String {
    let trimmed = model_name.trim();
    if label == "Grok" && trimmed.starts_with("Grok ") {
        return trimmed["Grok ".len()..].to_string();
    }
    if trimmed.is_empty() {
        "Auto".to_string()
    } else {
        trimmed.to_string()
    }
}

fn cyan(value: &str) -> String {
    format!("\u{001b}[36m{value}\u{001b}[0m")
}

fn yellow(value: &str) -> String {
    format!("\u{001b}[33m{value}\u{001b}[0m")
}

#[cfg(test)]
mod tests {
    use super::{CliHudState, lines, plain_lines};
    use crate::options::OutputFormat;
    use crate::terminal::strip_ansi;

    #[test]
    fn hud_renderer_shows_project_first_status_and_warnings_like_swift() {
        let mut state = CliHudState {
            model_name: "Expert".to_string(),
            workspace_name: Some("Research Notes".to_string()),
            private_mode: false,
            stream: true,
            output_format: OutputFormat::Markdown,
            attached_file_count: 2,
            rate_limit_warning: None,
        };

        let status = plain_lines(&state, 120).join("\n");
        assert!(status.contains("Research Notes > Expert | MD | 2 files"));
        assert!(!status.contains("model:"));
        assert!(!status.contains("workspace:"));
        assert!(!status.contains("/ commands"));

        state.private_mode = true;
        state.stream = false;
        state.rate_limit_warning = Some("2 left | reset 14m".to_string());
        let warning = plain_lines(&state, 120).join("\n");
        assert!(warning.contains("Research Notes > Private | Expert | MD | Stream off | 2 files"));
        assert!(warning.contains("Limit > 2 left | reset 14m"));

        let narrow = plain_lines(&state, 32).join("\n");
        assert!(narrow.lines().all(|line| line.chars().count() <= 32));
    }

    #[test]
    fn hud_renderer_trims_default_grok_model_prefix_like_swift() {
        let state = CliHudState {
            model_name: "Grok 4.3 (beta)".to_string(),
            workspace_name: None,
            private_mode: false,
            stream: true,
            output_format: OutputFormat::Markdown,
            attached_file_count: 0,
            rate_limit_warning: None,
        };

        assert_eq!(plain_lines(&state, 120), vec!["Grok > 4.3 (beta) | MD"]);
    }

    #[test]
    fn hud_renderer_keeps_grok_model_prefix_for_named_workspaces_like_swift() {
        let state = CliHudState {
            model_name: "Grok 4.3 (beta)".to_string(),
            workspace_name: Some("Research Notes".to_string()),
            private_mode: false,
            stream: true,
            output_format: OutputFormat::Raw,
            attached_file_count: 1,
            rate_limit_warning: Some(String::new()),
        };

        assert_eq!(
            plain_lines(&state, 120),
            vec!["Research Notes > Grok 4.3 (beta) | Raw | 1 file"]
        );
    }

    #[test]
    fn hud_renderer_colors_lines_but_plain_output_strips_to_status_like_swift() {
        let state = CliHudState {
            model_name: "  ".to_string(),
            workspace_name: None,
            private_mode: false,
            stream: false,
            output_format: OutputFormat::Json,
            attached_file_count: 0,
            rate_limit_warning: Some("nearly done".to_string()),
        };

        let colored = lines(&state, 120);
        assert!(colored.iter().all(|line| line.contains("\u{001b}[")));
        assert_eq!(
            colored
                .iter()
                .map(|line| strip_ansi(line))
                .collect::<Vec<_>>(),
            vec![
                "Grok > Auto | JSON | Stream off".to_string(),
                "Limit > nearly done".to_string(),
            ]
        );
    }
}
