use grok_client::GrokMode;

pub(crate) fn available_models_text(current_mode: Option<&GrokMode>, modes: &[GrokMode]) -> String {
    let mut lines = Vec::new();
    if let Some(current_mode) = current_mode {
        lines.push(format!(
            "Current model: {} ({})",
            current_mode.display_name, current_mode.id
        ));
    }
    lines.push("Available web modes:".to_string());

    for (index, mode) in modes.iter().enumerate() {
        let marker = if Some(mode.id.as_str()) == current_mode.map(|mode| mode.id.as_str()) {
            "✓ "
        } else {
            "  "
        };
        lines.push(model_list_line(mode, Some(index), marker));
    }
    lines.push("You can also pass a raw web modeId with --model.".to_string());
    lines.join("\n")
}

pub(crate) fn model_set_message(mode: &GrokMode) -> String {
    format!("Model set to: {} ({})", mode.display_name, mode.id)
}

fn model_list_line(mode: &GrokMode, index: Option<usize>, marker: &str) -> String {
    let number = index
        .map(|index| format!("{}. ", index + 1))
        .unwrap_or_default();
    let detail = if mode.summary.is_empty() {
        String::new()
    } else {
        format!(" - {}", mode.summary)
    };
    let unavailable = mode
        .unavailable_description()
        .map(|description| format!(" [unavailable: {description}]"))
        .unwrap_or_default();
    format!(
        "{marker}{number}{} ({}){detail}{unavailable}",
        mode.display_name, mode.id
    )
}

#[cfg(test)]
mod tests {
    use super::{available_models_text, model_list_line, model_set_message};
    use grok_client::GrokMode;

    #[test]
    fn available_models_text_matches_swift_model_list_shape() {
        let output = available_models_text(
            Some(&GrokMode::fast()),
            &[GrokMode::auto(), GrokMode::fast(), GrokMode::expert()],
        );

        assert!(output.contains("Current model: Fast (fast)"));
        assert!(output.contains("Available web modes:"));
        assert!(output.contains("✓ 2. Fast (fast) - Quick responses"));
        assert!(output.contains("  3. Expert (expert) - Thinks hard"));
        assert!(output.ends_with("You can also pass a raw web modeId with --model."));
    }

    #[test]
    fn available_models_text_can_omit_current_mode_like_swift() {
        let output = available_models_text(None, &[GrokMode::auto()]);

        assert!(!output.contains("Current model:"));
        assert!(output.starts_with("Available web modes:\n"));
        assert!(output.contains("  1. Auto (auto) - Chooses Fast or Expert"));
    }

    #[test]
    fn model_list_line_includes_unavailable_reason_like_swift() {
        let mut mode = GrokMode::with_summary("heavy", "Heavy", "Team of Experts");
        mode.is_available = false;
        mode.minimum_subscription_tier = Some("SuperGrok".to_string());

        assert_eq!(
            model_list_line(&mode, Some(0), "  "),
            "  1. Heavy (heavy) - Team of Experts [unavailable: Requires SuperGrok]"
        );

        mode.unavailable_reason = Some("Not enabled".to_string());
        assert_eq!(
            model_list_line(&mode, None, "> "),
            "> Heavy (heavy) - Team of Experts [unavailable: Not enabled]"
        );
    }

    #[test]
    fn model_set_message_matches_interactive_swift_status_text() {
        assert_eq!(
            model_set_message(&GrokMode::expert()),
            "Model set to: Expert (expert)"
        );
    }
}
