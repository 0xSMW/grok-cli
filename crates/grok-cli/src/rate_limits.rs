use grok_client::{GrokMode, GrokRateLimit};
use std::time::SystemTime;

pub fn rate_limit_summary(rate_limit: &GrokRateLimit, mode: &GrokMode) -> String {
    rate_limit_summary_at(rate_limit, mode, SystemTime::now())
}

pub fn unavailable_rate_limit_summary(mode: &GrokMode) -> String {
    [
        format!("Rate limits for {} ({})", mode.display_name, mode.id),
        "Current rate-limit data is unavailable.".to_string(),
    ]
    .join("\n")
}

pub fn rate_limit_status(rate_limit: &GrokRateLimit) -> Option<String> {
    rate_limit_status_at(rate_limit, SystemTime::now())
}

pub fn rate_limit_warning(rate_limit: &GrokRateLimit) -> Option<String> {
    rate_limit_warning_at(rate_limit, SystemTime::now())
}

fn rate_limit_summary_at(rate_limit: &GrokRateLimit, mode: &GrokMode, now: SystemTime) -> String {
    let mut lines = vec![format!(
        "Rate limits for {} ({})",
        mode.display_name, mode.id
    )];
    if let Some(model_name) = rate_limit.model_name.as_deref()
        && !model_name.is_empty()
        && model_name != mode.id
    {
        lines.push(format!("API model: {model_name}"));
    }

    if let Some(remaining) = rate_limit.remaining_responses {
        let noun = if remaining == 1 {
            "response"
        } else {
            "responses"
        };
        lines.push(format!("Remaining responses: {remaining} {noun}"));
    } else {
        lines.push("Remaining responses: unavailable".to_string());
    }

    if let Some(reset) = reset_duration_description_at(rate_limit, now) {
        lines.push(format!("Resets in {reset}"));
    } else if rate_limit.window_seconds.is_some() {
        lines.push(format!(
            "Resets as messages age out of the rolling {} window.",
            duration_description(rate_limit.window_seconds.unwrap_or_default())
        ));
    }

    lines.join("\n")
}

fn rate_limit_status_at(rate_limit: &GrokRateLimit, now: SystemTime) -> Option<String> {
    let remaining = low_remaining_responses(rate_limit)?;
    let mut status = format!("{remaining} left");
    if let Some(reset) = reset_duration_description_at(rate_limit, now) {
        status.push_str(&format!(" | reset {reset}"));
    }
    Some(status)
}

fn rate_limit_warning_at(rate_limit: &GrokRateLimit, now: SystemTime) -> Option<String> {
    let remaining = low_remaining_responses(rate_limit)?;
    let noun = if remaining == 1 {
        "response"
    } else {
        "responses"
    };
    let mut warning = format!("Warning: {remaining} {noun} remaining");
    if let Some(reset) = reset_duration_description_at(rate_limit, now) {
        warning.push_str(&format!("; resets in {reset}"));
    }
    warning.push('.');
    Some(warning)
}

fn low_remaining_responses(rate_limit: &GrokRateLimit) -> Option<i64> {
    rate_limit
        .remaining_responses
        .filter(|remaining| *remaining < 10)
}

fn reset_duration_description_at(rate_limit: &GrokRateLimit, now: SystemTime) -> Option<String> {
    rate_limit
        .seconds_until_reset(now)
        .map(duration_description)
}

fn duration_description(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds == 0 {
        return "now".to_string();
    }
    if seconds < 60 {
        return format!("{seconds}s");
    }

    let total_minutes = ((seconds as f64) / 60.0).ceil() as i64;
    if total_minutes < 60 {
        return format!("{total_minutes}m");
    }

    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;
    if hours < 24 {
        if minutes == 0 {
            return format!("{hours}h");
        }
        return format!("{hours}h {minutes}m");
    }

    let days = hours / 24;
    let remaining_hours = hours % 24;
    if remaining_hours == 0 {
        format!("{days}d")
    } else {
        format!("{days}d {remaining_hours}h")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        duration_description, rate_limit_status_at, rate_limit_summary_at, rate_limit_warning_at,
    };
    use grok_client::{GrokMode, GrokRateLimit};
    use serde_json::json;
    use std::time::{Duration, SystemTime};

    #[test]
    fn duration_description_matches_swift_rounding_edges() {
        assert_eq!(duration_description(-5), "now");
        assert_eq!(duration_description(0), "now");
        assert_eq!(duration_description(59), "59s");
        assert_eq!(duration_description(60), "1m");
        assert_eq!(duration_description(61), "2m");
        assert_eq!(duration_description(3_600), "1h");
        assert_eq!(duration_description(3_661), "1h 2m");
        assert_eq!(duration_description(86_400), "1d");
        assert_eq!(duration_description(90_000), "1d 1h");
    }

    #[test]
    fn rate_limit_summary_matches_swift_remaining_reset_and_model_lines() {
        let fetched_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let rate_limit = GrokRateLimit::from_raw_json_at(
            json!({
                "modelName": "grok-heavy",
                "remainingResponses": 1,
                "resetAfterSeconds": 901
            }),
            "fast",
            fetched_at,
        );

        assert_eq!(
            rate_limit_summary_at(
                &rate_limit,
                &GrokMode::fast(),
                fetched_at + Duration::from_secs(1)
            ),
            [
                "Rate limits for Fast (fast)",
                "API model: grok-heavy",
                "Remaining responses: 1 response",
                "Resets in 15m"
            ]
            .join("\n")
        );
    }

    #[test]
    fn rate_limit_summary_matches_swift_unavailable_and_rolling_window_text() {
        let fetched_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let rate_limit = GrokRateLimit::from_raw_json_at(
            json!({
                "modelName": "fast",
                "windowSeconds": 3600
            }),
            "fast",
            fetched_at,
        );

        assert_eq!(
            rate_limit_summary_at(&rate_limit, &GrokMode::fast(), fetched_at),
            [
                "Rate limits for Fast (fast)",
                "Remaining responses: unavailable",
                "Resets as messages age out of the rolling 1h window."
            ]
            .join("\n")
        );
    }

    #[test]
    fn rate_limit_summary_reports_reset_now_like_swift_duration_text() {
        let fetched_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let rate_limit = GrokRateLimit::from_raw_json_at(
            json!({
                "remainingResponses": 0,
                "resetAfterSeconds": 0
            }),
            "fast",
            fetched_at,
        );

        assert_eq!(
            rate_limit_summary_at(&rate_limit, &GrokMode::fast(), fetched_at),
            [
                "Rate limits for Fast (fast)",
                "Remaining responses: 0 responses",
                "Resets in now"
            ]
            .join("\n")
        );
    }

    #[test]
    fn rate_limit_status_matches_swift_low_limit_hud_text() {
        let fetched_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let rate_limit = GrokRateLimit::from_raw_json_at(
            json!({
                "remainingResponses": 2,
                "resetAfterSeconds": 901
            }),
            "fast",
            fetched_at,
        );

        assert_eq!(
            rate_limit_status_at(&rate_limit, fetched_at + Duration::from_secs(1)).as_deref(),
            Some("2 left | reset 15m")
        );

        let no_reset = GrokRateLimit::from_raw_json_at(
            json!({
                "remainingResponses": 9
            }),
            "fast",
            fetched_at,
        );
        assert_eq!(
            rate_limit_status_at(&no_reset, fetched_at).as_deref(),
            Some("9 left")
        );
    }

    #[test]
    fn rate_limit_status_ignores_non_low_or_missing_remaining_like_swift() {
        let fetched_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let ten_remaining = GrokRateLimit::from_raw_json_at(
            json!({
                "remainingResponses": 10,
                "resetAfterSeconds": 60
            }),
            "fast",
            fetched_at,
        );
        let missing_remaining = GrokRateLimit::from_raw_json_at(
            json!({
                "resetAfterSeconds": 60
            }),
            "fast",
            fetched_at,
        );

        assert_eq!(rate_limit_status_at(&ten_remaining, fetched_at), None);
        assert_eq!(rate_limit_status_at(&missing_remaining, fetched_at), None);
    }

    #[test]
    fn rate_limit_warning_matches_swift_sentence_and_pluralization() {
        let fetched_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let one_remaining = GrokRateLimit::from_raw_json_at(
            json!({
                "remainingResponses": 1,
                "resetAfterSeconds": 0
            }),
            "fast",
            fetched_at,
        );
        let two_remaining = GrokRateLimit::from_raw_json_at(
            json!({
                "remainingResponses": 2,
                "resetAfterSeconds": 901
            }),
            "fast",
            fetched_at,
        );
        let no_reset = GrokRateLimit::from_raw_json_at(
            json!({
                "remainingResponses": 9
            }),
            "fast",
            fetched_at,
        );

        assert_eq!(
            rate_limit_warning_at(&one_remaining, fetched_at).as_deref(),
            Some("Warning: 1 response remaining; resets in now.")
        );
        assert_eq!(
            rate_limit_warning_at(&two_remaining, fetched_at + Duration::from_secs(1)).as_deref(),
            Some("Warning: 2 responses remaining; resets in 15m.")
        );
        assert_eq!(
            rate_limit_warning_at(&no_reset, fetched_at).as_deref(),
            Some("Warning: 9 responses remaining.")
        );
    }

    #[test]
    fn unavailable_rate_limit_summary_matches_swift_fallback() {
        assert_eq!(
            super::unavailable_rate_limit_summary(&GrokMode::fast()),
            [
                "Rate limits for Fast (fast)",
                "Current rate-limit data is unavailable."
            ]
            .join("\n")
        );
    }
}
