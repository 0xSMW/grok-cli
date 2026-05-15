use serde_json::{Map, Value};

use crate::{GrokClient, GrokError, JsonLookup, Result};

#[derive(Clone, Debug, PartialEq)]
pub struct GrokTask {
    pub task_id: Option<String>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub prompt: Option<String>,
    pub is_enabled: Option<bool>,
    pub status: Option<String>,
    pub raw_json: Value,
}

impl GrokTask {
    pub fn resolved_id(&self) -> Option<&str> {
        self.task_id.as_deref().or(self.id.as_deref())
    }

    pub fn display_name(&self) -> Option<&str> {
        self.name.as_deref().or(self.prompt.as_deref())
    }

    fn from_dictionary(dictionary: Map<String, Value>, default_is_enabled: Option<bool>) -> Self {
        let mut raw_json = first_nested_dictionary(&dictionary, TASK_WRAPPER_KEYS)
            .map(Value::Object)
            .unwrap_or_else(|| Value::Object(dictionary));
        copy_schedule_fields(&mut raw_json);
        let mut lookup = JsonLookup::new(raw_json.clone());
        if lookup.bool(TASK_ENABLED_KEYS).is_none()
            && task_bool_value(&raw_json, TASK_ENABLED_KEYS).is_none()
            && let Some(default_is_enabled) = default_is_enabled
            && let Some(dictionary) = raw_json.as_object_mut()
        {
            dictionary.insert("isEnabled".to_string(), Value::Bool(default_is_enabled));
            lookup = JsonLookup::new(raw_json.clone());
        }

        let task_id = lookup.string(&["taskId", "task_id"]);
        let id = lookup.string(&["id"]);
        let name = lookup.string(&["name", "title", "displayName", "display_name"]);
        let prompt = lookup.string(&[
            "prompt",
            "taskPrompt",
            "task_prompt",
            "description",
            "summary",
            "query",
            "instructions",
        ]);
        let is_enabled = task_bool_value(&raw_json, TASK_ENABLED_KEYS)
            .or_else(|| lookup.bool(TASK_ENABLED_KEYS));
        let status = lookup.string(&["status", "state"]);

        Self {
            task_id,
            id,
            name,
            prompt,
            is_enabled,
            status,
            raw_json,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokTasksResponse {
    pub tasks: Vec<GrokTask>,
    pub active_tasks: Vec<GrokTask>,
    pub inactive_tasks: Vec<GrokTask>,
    pub raw_json: Value,
}

impl GrokTasksResponse {
    pub fn from_raw_json(raw_json: Value, default_is_enabled: Option<bool>) -> Self {
        let active_tasks = dictionaries_from_keys(&raw_json, ACTIVE_TASK_KEYS)
            .into_iter()
            .map(|dictionary| GrokTask::from_dictionary(dictionary, Some(true)))
            .collect::<Vec<_>>();
        let inactive_tasks = dictionaries_from_keys(&raw_json, INACTIVE_TASK_KEYS)
            .into_iter()
            .map(|dictionary| GrokTask::from_dictionary(dictionary, Some(false)))
            .collect::<Vec<_>>();
        let tasks = if active_tasks.is_empty() && inactive_tasks.is_empty() {
            dictionaries_from_keys(&raw_json, GENERAL_TASK_KEYS)
                .into_iter()
                .map(|dictionary| GrokTask::from_dictionary(dictionary, default_is_enabled))
                .collect::<Vec<_>>()
        } else {
            active_tasks
                .iter()
                .chain(inactive_tasks.iter())
                .cloned()
                .collect()
        };

        Self {
            tasks,
            active_tasks,
            inactive_tasks,
            raw_json,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokTaskMutationResponse {
    pub task: Option<GrokTask>,
    pub raw_json: Value,
}

impl GrokTaskMutationResponse {
    pub fn from_raw_json(raw_json: Value) -> Self {
        let task = first_task_mutation_dictionary(&raw_json)
            .map(|dictionary| GrokTask::from_dictionary(dictionary, None));
        Self { task, raw_json }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokTaskResult {
    pub result_id: Option<String>,
    pub id: Option<String>,
    pub task_id: Option<String>,
    pub conversation_id: Option<String>,
    pub response_id: Option<String>,
    pub message: Option<String>,
    pub status: Option<String>,
    pub raw_json: Value,
}

impl GrokTaskResult {
    pub fn resolved_id(&self) -> Option<&str> {
        self.result_id.as_deref().or(self.id.as_deref())
    }

    fn from_dictionary(dictionary: Map<String, Value>) -> Self {
        let raw_json = Value::Object(dictionary);
        let lookup = JsonLookup::new(raw_json.clone());
        Self {
            result_id: lookup.string(&["taskResultId", "task_result_id", "resultId", "result_id"]),
            id: lookup.string(&["id"]),
            task_id: lookup.string(&["taskId", "task_id"]),
            conversation_id: lookup.string(&["conversationId", "conversation_id"]),
            response_id: lookup.string(&["responseId", "response_id"]),
            message: task_result_message(&raw_json),
            status: lookup.string(&["status", "state"]),
            raw_json,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokTaskResultsResponse {
    pub results: Vec<GrokTaskResult>,
    pub raw_json: Value,
}

impl GrokTaskResultsResponse {
    pub fn from_raw_json(raw_json: Value) -> Self {
        let mut dictionaries = dictionaries_from_keys(&raw_json, TASK_RESULT_KEYS);
        if dictionaries.is_empty()
            && let Some(dictionary) = first_task_result_dictionary(&raw_json)
        {
            dictionaries.push(dictionary);
        }
        Self {
            results: dictionaries
                .into_iter()
                .map(GrokTaskResult::from_dictionary)
                .collect(),
            raw_json,
        }
    }
}

impl GrokClient {
    pub async fn list_tasks_response(&self) -> Result<GrokTasksResponse> {
        let request = self.list_tasks_request()?;
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokTasksResponse::from_raw_json(raw_json, None))
    }

    pub async fn list_inactive_tasks_response(&self) -> Result<GrokTasksResponse> {
        let request = self.list_inactive_tasks_request()?;
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokTasksResponse::from_raw_json(raw_json, Some(false)))
    }

    pub async fn task_results_response(
        &self,
        task_id: &str,
        limit: usize,
    ) -> Result<GrokTaskResultsResponse> {
        let request = self.task_results_request(task_id, limit)?;
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokTaskResultsResponse::from_raw_json(raw_json))
    }

    pub async fn latest_task_result(&self, task_id: &str) -> Result<Option<GrokTaskResult>> {
        Ok(self
            .task_results_response(task_id, 1)
            .await?
            .results
            .into_iter()
            .next())
    }

    pub async fn create_task(
        &self,
        prompt: &str,
        options: &crate::GrokTaskCreateOptions,
    ) -> Result<GrokTaskMutationResponse> {
        let request = self.create_task_request(prompt, options)?;
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokTaskMutationResponse::from_raw_json(raw_json))
    }

    pub async fn archive_task(
        &self,
        task_id: &str,
        is_enabled: bool,
    ) -> Result<GrokTaskMutationResponse> {
        let request = self.archive_task_request(task_id, is_enabled)?;
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokTaskMutationResponse::from_raw_json(raw_json))
    }
}

const ACTIVE_TASK_KEYS: &[&str] = &[
    "activeTasks",
    "active_tasks",
    "enabledTasks",
    "enabled_tasks",
    "active",
    "enabled",
];
const INACTIVE_TASK_KEYS: &[&str] = &[
    "inactiveTasks",
    "inactive_tasks",
    "archivedTasks",
    "archived_tasks",
    "disabledTasks",
    "disabled_tasks",
    "inactive",
    "archived",
    "disabled",
];
const GENERAL_TASK_KEYS: &[&str] = &[
    "tasks",
    "taskList",
    "task_list",
    "data",
    "result",
    "items",
    "values",
];
const TASK_WRAPPER_KEYS: &[&str] = &["task", "taskData", "task_data"];
const TASK_RESULT_KEYS: &[&str] = &[
    "results",
    "taskResults",
    "task_results",
    "data",
    "result",
    "items",
    "values",
];
const TASK_RESULT_WRAPPER_KEYS: &[&str] = &[
    "results",
    "taskResults",
    "task_results",
    "data",
    "result",
    "items",
    "values",
    "taskResult",
    "task_result",
    "latestResult",
    "latest_result",
    "lastResult",
    "last_result",
    "response",
    "modelResponse",
    "model_response",
];
const TASK_ENABLED_KEYS: &[&str] = &[
    "isEnabled",
    "is_enabled",
    "isActive",
    "is_active",
    "enabled",
    "active",
];

fn dictionaries_from_keys(value: &Value, keys: &[&str]) -> Vec<Map<String, Value>> {
    let dictionaries = JsonLookup::new(value.clone()).all_dictionaries(keys);
    if dictionaries.iter().any(has_task_or_result_identity) {
        return dictionaries
            .into_iter()
            .filter(has_task_or_result_identity)
            .collect();
    }

    dictionaries
        .into_iter()
        .flat_map(|dictionary| dictionaries_from_keys(&Value::Object(dictionary), keys))
        .collect()
}

fn first_task_mutation_dictionary(value: &Value) -> Option<Map<String, Value>> {
    let lookup = JsonLookup::new(value.clone());
    lookup
        .first_dictionary(TASK_WRAPPER_KEYS)
        .or_else(|| value.as_object().cloned())
}

fn first_task_result_dictionary(value: &Value) -> Option<Map<String, Value>> {
    match value {
        Value::Array(values) => values.iter().find_map(first_task_result_dictionary),
        Value::Object(dictionary) => {
            if is_task_result_dictionary(dictionary) {
                return Some(dictionary.clone());
            }
            for key in TASK_RESULT_WRAPPER_KEYS {
                if let Some(nested) = dictionary.get(*key)
                    && let Some(result) = first_task_result_dictionary(nested)
                {
                    return Some(result);
                }
            }
            None
        }
        _ => None,
    }
}

fn first_nested_dictionary(
    dictionary: &Map<String, Value>,
    keys: &[&str],
) -> Option<Map<String, Value>> {
    keys.iter()
        .find_map(|key| dictionary.get(*key).and_then(Value::as_object).cloned())
}

fn copy_schedule_fields(raw_json: &mut Value) {
    let Some(dictionary) = raw_json.as_object_mut() else {
        return;
    };
    let schedule = dictionary
        .get("schedule")
        .and_then(Value::as_object)
        .cloned()
        .or_else(|| {
            dictionary
                .get("schedules")
                .and_then(Value::as_array)
                .and_then(|schedules| schedules.iter().find_map(Value::as_object))
                .cloned()
        });
    let Some(schedule) = schedule else {
        return;
    };

    if !dictionary.contains_key("scheduleId") {
        for key in ["scheduleId", "schedule_id"] {
            if let Some(value) = schedule.get(key) {
                dictionary.insert("scheduleId".to_string(), value.clone());
                break;
            }
        }
    }
    for key in [
        "dayOfYear",
        "date",
        "timeOfDay",
        "time",
        "timezone",
        "timeZone",
        "nextRun",
    ] {
        if !dictionary.contains_key(key)
            && let Some(value) = schedule.get(key)
        {
            dictionary.insert(key.to_string(), value.clone());
        }
    }
    for key in ["isEnabled", "is_enabled", "enabled"] {
        if let Some(value) = schedule.get(key) {
            dictionary.insert("scheduleIsEnabled".to_string(), value.clone());
            break;
        }
    }
    let has_task_enabled_value = TASK_ENABLED_KEYS
        .iter()
        .any(|key| dictionary.get(*key).and_then(bool_value).is_some());
    if !has_task_enabled_value
        && ["isEnabled", "is_enabled", "enabled"]
            .iter()
            .find_map(|key| schedule.get(*key).and_then(bool_value))
            == Some(true)
    {
        dictionary.insert("isEnabled".to_string(), Value::Bool(true));
    }
}

fn has_task_or_result_identity(dictionary: &Map<String, Value>) -> bool {
    dictionary.contains_key("taskId")
        || dictionary.contains_key("task_id")
        || dictionary.contains_key("id")
        || dictionary.contains_key("task")
        || dictionary.contains_key("taskData")
        || dictionary.contains_key("task_data")
        || dictionary.contains_key("name")
        || dictionary.contains_key("title")
        || dictionary.contains_key("prompt")
        || dictionary.contains_key("taskPrompt")
        || dictionary.contains_key("task_prompt")
        || is_task_result_dictionary(dictionary)
}

fn is_task_result_dictionary(dictionary: &Map<String, Value>) -> bool {
    [
        "taskResultId",
        "task_result_id",
        "resultId",
        "result_id",
        "id",
        "taskId",
        "task_id",
        "conversationId",
        "conversation_id",
        "responseId",
        "response_id",
        "summary",
        "message",
        "content",
        "output",
        "text",
    ]
    .iter()
    .any(|key| dictionary.contains_key(*key))
        || task_result_message(&Value::Object(dictionary.clone())).is_some()
}

fn task_result_message(value: &Value) -> Option<String> {
    let dictionary = value.as_object()?;

    for key in ["summary", "message", "content", "output", "text", "result"] {
        if let Some(text) = dictionary.get(key).and_then(non_empty_string_value) {
            return Some(text);
        }
    }

    for key in [
        "result",
        "response",
        "modelResponse",
        "model_response",
        "conversation",
    ] {
        if let Some(text) = dictionary.get(key).and_then(task_result_message) {
            return Some(text);
        }
    }

    for key in ["messages", "responses", "contents", "parts"] {
        let Some(array) = dictionary.get(key).and_then(Value::as_array) else {
            continue;
        };
        for item in array {
            if let Some(text) = task_result_message(item).or_else(|| non_empty_string_value(item)) {
                return Some(text);
            }
        }
    }

    None
}

fn non_empty_string_value(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn task_bool_value(value: &Value, keys: &[&str]) -> Option<bool> {
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
            "false" | "no" | "0" | "disabled" | "inactive" | "archived" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn require_task_schedule(
    options: &crate::GrokTaskCreateOptions,
) -> Result<&crate::options::GrokTaskSchedule> {
    options
        .schedule
        .as_ref()
        .ok_or_else(|| GrokError::Api("Task create options must include a schedule".to_string()))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{GrokTaskResultsResponse, GrokTasksResponse};

    #[test]
    fn parses_tasks_with_active_and_inactive_wrappers() {
        let response = GrokTasksResponse::from_raw_json(
            json!({
                "tasks": {
                    "active": [
                        {
                            "task_id": "active-1",
                            "title": "Morning brief",
                            "task_prompt": "Summarize news",
                            "schedule": {
                                "timeOfDay": "08:00",
                                "timezone": "America/New_York",
                                "isEnabled": true
                            }
                        }
                    ],
                    "archived_tasks": [
                        {
                            "taskId": "archived-1",
                            "name": "Old task",
                            "prompt": "Done",
                            "status": "TASK_ARCHIVED"
                        }
                    ]
                }
            }),
            None,
        );

        assert_eq!(response.tasks.len(), 2);
        assert_eq!(response.tasks[0].resolved_id(), Some("active-1"));
        assert_eq!(response.tasks[0].display_name(), Some("Morning brief"));
        assert_eq!(response.tasks[0].is_enabled, Some(true));
        assert_eq!(response.tasks[1].resolved_id(), Some("archived-1"));
        assert_eq!(response.tasks[1].is_enabled, Some(false));
    }

    #[test]
    fn promotes_nested_enabled_schedule_to_task_enabled_like_swift() {
        let response = GrokTasksResponse::from_raw_json(
            json!({
                "tasks": [
                    {
                        "taskId": "scheduled-1",
                        "name": "Morning brief",
                        "schedule": {
                            "isEnabled": true,
                            "timeOfDay": "08:00",
                            "timezone": "America/New_York"
                        }
                    },
                    {
                        "taskId": "scheduled-2",
                        "name": "Paused schedule",
                        "schedule": {
                            "isEnabled": false
                        }
                    },
                    {
                        "taskId": "scheduled-3",
                        "name": "Explicitly disabled",
                        "isEnabled": false,
                        "schedule": {
                            "isEnabled": true
                        }
                    }
                ]
            }),
            None,
        );

        assert_eq!(response.tasks.len(), 3);
        assert_eq!(response.tasks[0].is_enabled, Some(true));
        assert_eq!(
            response.tasks[0].raw_json["scheduleIsEnabled"],
            Value::Bool(true)
        );
        assert_eq!(response.tasks[1].is_enabled, None);
        assert_eq!(
            response.tasks[1].raw_json["scheduleIsEnabled"],
            Value::Bool(false)
        );
        assert_eq!(response.tasks[2].is_enabled, Some(false));
    }

    #[test]
    fn parses_task_results_from_common_wrappers() {
        let response = GrokTaskResultsResponse::from_raw_json(json!({
            "data": {
                "items": [
                    {
                        "taskResultId": "result-1",
                        "taskId": "task-1",
                        "conversationId": "conv-1",
                        "responseId": "resp-1",
                        "summary": "Finished",
                        "state": "TASK_RESULT_DONE"
                    }
                ]
            }
        }));

        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].resolved_id(), Some("result-1"));
        assert_eq!(
            response.results[0].conversation_id.as_deref(),
            Some("conv-1")
        );
        assert_eq!(response.results[0].message.as_deref(), Some("Finished"));
    }

    #[test]
    fn parses_task_result_message_from_nested_model_response_like_swift() {
        let response = GrokTaskResultsResponse::from_raw_json(json!({
            "results": [
                {
                    "taskResultId": "result-1",
                    "taskId": "task-1",
                    "response": {
                        "modelResponse": {
                            "message": "Nested task summary"
                        }
                    }
                }
            ]
        }));

        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].resolved_id(), Some("result-1"));
        assert_eq!(
            response.results[0].message.as_deref(),
            Some("Nested task summary")
        );
    }

    #[test]
    fn parses_task_result_message_from_array_string_fallback_like_swift() {
        let response = GrokTaskResultsResponse::from_raw_json(json!({
            "results": [
                {
                    "id": "result-2",
                    "parts": [
                        {
                            "response": {
                                "message": "   "
                            }
                        },
                        "Array fallback summary"
                    ]
                }
            ]
        }));

        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].resolved_id(), Some("result-2"));
        assert_eq!(
            response.results[0].message.as_deref(),
            Some("Array fallback summary")
        );
    }
}
