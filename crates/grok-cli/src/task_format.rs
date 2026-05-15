use grok_client::GrokTask;
use serde_json::Value;

pub(crate) fn task_status(task: &GrokTask) -> Option<String> {
    let raw_status = task
        .status
        .as_deref()
        .and_then(compact_task_status)
        .or_else(|| {
            string_in_value(&task.raw_json, &["status", "state"])
                .as_deref()
                .and_then(compact_task_status)
        });
    let is_enabled = task
        .is_enabled
        .or_else(|| bool_in_value(&task.raw_json, &["isEnabled", "is_enabled", "enabled"]));

    if is_enabled == Some(false) {
        return raw_status.or_else(|| Some("archived".to_string()));
    }
    if schedule_enabled(&task.raw_json) == Some(false) {
        return Some("paused".to_string());
    }
    raw_status.or_else(|| enabled_status(is_enabled))
}

pub(crate) fn schedule_value(raw: &Value) -> Option<String> {
    for key in ["schedule", "scheduledTime", "scheduledAt"] {
        if let Some(value) = raw
            .as_object()
            .and_then(|dictionary| dictionary.get(key))
            .and_then(scalar_string_value)
            .filter(|value| !value.is_empty())
        {
            return Some(value);
        }
    }

    let schedule = raw
        .as_object()
        .and_then(|dictionary| dictionary.get("schedule"))
        .and_then(Value::as_object)
        .cloned()
        .map(Value::Object)
        .unwrap_or_else(|| raw.clone());
    let parts = [
        string_in_value(&schedule, &["dayOfYear", "date"]),
        string_in_value(&schedule, &["timeOfDay", "time"]),
        string_in_value(&schedule, &["timezone", "timeZone"]),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();

    (!parts.is_empty()).then(|| parts.join(" "))
}

pub(crate) fn compact_task_status(value: &str) -> Option<String> {
    let mut status = value.trim().to_string();
    if status.is_empty() {
        return None;
    }
    for prefix in ["TASK_RESULT_", "TASK_", "RESULT_"] {
        if status.to_uppercase().starts_with(prefix) {
            status = status[prefix.len()..].to_string();
            break;
        }
    }
    Some(status.replace('_', " ").to_lowercase())
}

fn enabled_status(is_enabled: Option<bool>) -> Option<String> {
    is_enabled.map(|is_enabled| {
        if is_enabled {
            "enabled".to_string()
        } else {
            "archived".to_string()
        }
    })
}

pub(crate) fn schedule_enabled(raw: &Value) -> Option<bool> {
    if let Some(value) = bool_in_value(
        raw,
        &[
            "scheduleIsEnabled",
            "schedule_is_enabled",
            "isScheduleEnabled",
            "is_schedule_enabled",
        ],
    ) {
        return Some(value);
    }

    let dictionary = raw.as_object()?;
    if let Some(schedule) = dictionary.get("schedule")
        && let Some(value) = bool_in_value(schedule, &["isEnabled", "is_enabled", "enabled"])
    {
        return Some(value);
    }

    dictionary
        .get("schedules")
        .and_then(Value::as_array)
        .and_then(|schedules| {
            schedules.iter().find_map(|schedule| {
                bool_in_value(schedule, &["isEnabled", "is_enabled", "enabled"])
            })
        })
}

fn string_in_value(value: &Value, keys: &[&str]) -> Option<String> {
    let dictionary = value.as_object()?;
    keys.iter().find_map(|key| {
        dictionary
            .get(*key)
            .and_then(scalar_string_value)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn scalar_string_value(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
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

#[cfg(test)]
mod tests {
    use super::{compact_task_status, schedule_value, task_status};
    use grok_client::GrokTask;
    use serde_json::{Value, json};

    #[test]
    fn compact_task_status_matches_swift_prefix_cleanup() {
        assert_eq!(
            compact_task_status("TASK_RESULT_DONE"),
            Some("done".to_string())
        );
        assert_eq!(
            compact_task_status(" TASK_WAITING_FOR_USER "),
            Some("waiting for user".to_string())
        );
        assert_eq!(
            compact_task_status("RESULT_NEEDS_REVIEW"),
            Some("needs review".to_string())
        );
        assert_eq!(compact_task_status("   "), None);
    }

    #[test]
    fn schedule_value_matches_swift_direct_and_nested_priority() {
        assert_eq!(
            schedule_value(&json!({"schedule": "weekday mornings"})),
            Some("weekday mornings".to_string())
        );
        assert_eq!(
            schedule_value(&json!({"scheduledTime": "2026-05-15 08:00"})),
            Some("2026-05-15 08:00".to_string())
        );
        assert_eq!(
            schedule_value(
                &json!({"schedule": {"date": "2026-05-15", "time": "08:00", "timeZone": "Asia/Bangkok"}})
            ),
            Some("2026-05-15 08:00 Asia/Bangkok".to_string())
        );
        assert_eq!(
            schedule_value(&json!({"dayOfYear": "2026-05-15", "timeOfDay": "08:00"})),
            Some("2026-05-15 08:00".to_string())
        );
        assert_eq!(schedule_value(&json!({"nextRun": "tomorrow"})), None);
    }

    #[test]
    fn task_status_matches_swift_enabled_schedule_and_raw_status_order() {
        assert_eq!(
            task_status(&task(json!({"isEnabled": true}))),
            Some("enabled".to_string())
        );
        assert_eq!(
            task_status(&task(json!({"isEnabled": false}))),
            Some("archived".to_string())
        );
        assert_eq!(
            task_status(&task(
                json!({"isEnabled": true, "schedule": {"isEnabled": false}})
            )),
            Some("paused".to_string())
        );
        assert_eq!(
            task_status(&task(json!({"state": "TASK_RESULT_DONE"}))),
            Some("done".to_string())
        );
        assert_eq!(
            task_status(&task(json!({"state": "TASK_ARCHIVED", "isEnabled": false}))),
            Some("archived".to_string())
        );
    }

    #[test]
    fn task_status_treats_paused_string_as_false_like_swift_bool_value() {
        assert_eq!(
            task_status(&task(json!({"isEnabled": "paused"}))),
            Some("archived".to_string())
        );
        assert_eq!(
            task_status(&task(json!({"scheduleIsEnabled": "paused"}))),
            Some("paused".to_string())
        );
    }

    fn task(raw_json: Value) -> GrokTask {
        GrokTask {
            task_id: None,
            id: None,
            name: None,
            prompt: None,
            is_enabled: None,
            status: None,
            raw_json,
        }
    }
}
