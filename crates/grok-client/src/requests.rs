use reqwest::Method;
use serde_json::{Value, json};

use crate::{
    GrokAssetListOptions, GrokClient, GrokConversationListOptions, GrokError, GrokRequest,
    GrokSpeechToTextOptions, GrokTaskCreateOptions, GrokWorkspaceCreateOptions,
    GrokWorkspaceListOptions, QueryItem, RestNamespace, Result, endpoint_path,
};

pub const DEFAULT_SPEECH_REFINEMENT_LEVEL: &str = "REFINEMENT_LEVEL_POLISH";

pub fn infer_audio_format(file_name: &str) -> Option<&'static str> {
    let extension = file_name
        .rsplit_once('.')
        .map(|(_, extension)| extension.trim().to_lowercase())?;

    match extension.as_str() {
        "webm" => Some("webm"),
        "wav" => Some("wav"),
        "mp3" => Some("mp3"),
        "m4a" => Some("m4a"),
        "ogg" => Some("ogg"),
        "flac" => Some("flac"),
        "mp4" => Some("mp4"),
        "mpeg" => Some("mpeg"),
        "mpga" => Some("mpga"),
        _ => None,
    }
}

impl GrokClient {
    pub fn typeahead_request(
        &self,
        query: &str,
        lang: &str,
        max_items: usize,
        platform: &str,
        source: usize,
    ) -> Result<Option<GrokRequest>> {
        let trimmed_query = query.trim();
        if trimmed_query.is_empty() {
            return Ok(None);
        }

        let path = endpoint_path(
            &["_worker", "typeahead"],
            &[
                QueryItem::new("lang", lang),
                QueryItem::new("maxItems", max_items.to_string()),
                QueryItem::new("q", trimmed_query),
                QueryItem::new("platform", platform),
                QueryItem::new("source", source.to_string()),
            ],
        )?;
        self.make_request(&path, Method::GET, None, RestNamespace::Web)
            .map(Some)
    }

    pub fn list_modes_request(&self) -> Result<GrokRequest> {
        self.make_request(
            "/modes",
            Method::POST,
            Some(Value::Object(Default::default())),
            RestNamespace::Root,
        )
    }

    pub fn subscriptions_request(&self) -> Result<GrokRequest> {
        self.make_request("/subscriptions", Method::GET, None, RestNamespace::Root)
    }

    pub fn rate_limits_request(&self, model_name: &str) -> Result<GrokRequest> {
        let resolved_model_name = if model_name.trim().is_empty() {
            crate::GrokMode::default_mode().id
        } else {
            model_name.trim().to_string()
        };
        self.make_request(
            "/rate-limits",
            Method::POST,
            Some(json!({ "modelName": resolved_model_name })),
            RestNamespace::Root,
        )
    }

    pub fn list_skills_request(&self, locale: &str) -> Result<GrokRequest> {
        self.make_request(
            "/skills",
            Method::POST,
            Some(json!({ "locale": locale })),
            RestNamespace::Root,
        )
    }

    pub fn list_user_skills_request(&self) -> Result<GrokRequest> {
        self.make_request("/user-skills", Method::GET, None, RestNamespace::Root)
    }

    pub fn user_settings_request(&self) -> Result<GrokRequest> {
        self.make_request("/user-settings", Method::GET, None, RestNamespace::Root)
    }

    pub fn update_agent_customizations_request(
        &self,
        customizations: &[crate::GrokAgentCustomization],
    ) -> Result<GrokRequest> {
        let mut sorted_customizations = customizations.to_vec();
        sorted_customizations.sort_by_key(|customization| customization.agent_id);
        let values = sorted_customizations
            .iter()
            .map(|customization| {
                json!({
                    "agentId": customization.agent_id,
                    "name": if customization.agent_id == 0 {
                        "Grok"
                    } else {
                        customization.name.as_str()
                    },
                    "instructions": customization.instructions
                })
            })
            .collect::<Vec<_>>();
        self.make_request(
            "/user-settings",
            Method::POST,
            Some(json!({ "agentCustomizations": { "values": values } })),
            RestNamespace::Root,
        )
    }

    pub fn list_tasks_request(&self) -> Result<GrokRequest> {
        self.make_request("/tasks", Method::GET, None, RestNamespace::Root)
    }

    pub fn list_inactive_tasks_request(&self) -> Result<GrokRequest> {
        self.make_request("/tasks/inactive", Method::GET, None, RestNamespace::Root)
    }

    pub fn task_results_request(&self, task_id: &str, limit: usize) -> Result<GrokRequest> {
        let path = endpoint_path(
            &["tasks", "results", task_id],
            &[QueryItem::new("limit", limit.to_string())],
        )?;
        self.make_request(&path, Method::GET, None, RestNamespace::Root)
    }

    pub fn create_task_request(
        &self,
        prompt: &str,
        options: &GrokTaskCreateOptions,
    ) -> Result<GrokRequest> {
        let schedule = crate::tasks::require_task_schedule(options)?;
        self.make_request(
            "/tasks",
            Method::POST,
            Some(json!({
                "name": options.name,
                "prompt": prompt,
                "metadataJsonString": options.metadata_json_string,
                "schedule": {
                    "taskCadence": schedule.task_cadence,
                    "isEnabled": schedule.is_enabled,
                    "timezone": schedule.timezone,
                    "timeOfDay": schedule.time_of_day,
                    "dayOfYear": schedule.day_of_year
                },
                "notificationMethod": options.notification_method,
                "modelMode": options.model_mode,
                "notificationDeciderEnable": options.notification_decider_enable,
                "notificationDeciderGuideline": options.notification_decider_guideline,
                "modelName": options.model_name,
                "toolset": options.toolset
            })),
            RestNamespace::Root,
        )
    }

    pub fn archive_task_request(&self, task_id: &str, is_enabled: bool) -> Result<GrokRequest> {
        self.make_request(
            "/tasks/archive",
            Method::PUT,
            Some(json!({ "taskId": task_id, "isEnabled": is_enabled })),
            RestNamespace::Root,
        )
    }

    pub fn share_links_request(
        &self,
        conversation_id: &str,
        response_id: Option<&str>,
        options: &crate::GrokShareLinkOptions,
    ) -> Result<GrokRequest> {
        let mut query_items = vec![
            QueryItem::new("pageSize", options.page_size.to_string()),
            QueryItem::new("conversationId", conversation_id),
        ];
        if let Some(response_id) = response_id.filter(|value| !value.is_empty()) {
            query_items.push(QueryItem::new("responseId", response_id));
        }
        let path = endpoint_path(&["share_links"], &query_items)?;
        self.make_request(&path, Method::GET, None, RestNamespace::AppChat)
    }

    pub fn create_share_link_request(
        &self,
        conversation_id: &str,
        response_id: &str,
        options: &crate::GrokShareLinkOptions,
    ) -> Result<GrokRequest> {
        let path = endpoint_path(&["conversations", conversation_id, "share"], &[])?;
        self.make_request(
            &path,
            Method::POST,
            Some(json!({
                "responseId": response_id,
                "allowIndexing": options.allow_indexing
            })),
            RestNamespace::AppChat,
        )
    }

    pub fn list_conversations_request(
        &self,
        options: &GrokConversationListOptions,
    ) -> Result<GrokRequest> {
        let mut query_items = vec![QueryItem::new("pageSize", options.page_size.to_string())];
        if let Some(search_query) = options.search_query.as_deref()
            && !search_query.trim().is_empty()
        {
            query_items.push(QueryItem::new("searchQuery", search_query.trim()));
        }

        let path = endpoint_path(&["conversations"], &query_items)?;
        self.make_request(&path, Method::GET, None, RestNamespace::AppChat)
    }

    pub fn soft_delete_conversation_request(&self, conversation_id: &str) -> Result<GrokRequest> {
        let path = endpoint_path(&["conversations", "soft", conversation_id], &[])?;
        self.make_request(&path, Method::DELETE, None, RestNamespace::AppChat)
    }

    pub fn response_nodes_request(
        &self,
        conversation_id: &str,
        include_threads: bool,
    ) -> Result<GrokRequest> {
        let query_items = if include_threads {
            vec![QueryItem::new("includeThreads", "true")]
        } else {
            Vec::new()
        };
        let path = endpoint_path(
            &["conversations", conversation_id, "response-node"],
            &query_items,
        )?;
        self.make_request(&path, Method::GET, None, RestNamespace::AppChat)
    }

    pub fn load_responses_request(
        &self,
        conversation_id: &str,
        response_ids: &[String],
    ) -> Result<GrokRequest> {
        let body = if response_ids.is_empty() {
            Value::Object(Default::default())
        } else {
            json!({ "responseIds": response_ids })
        };
        let path = endpoint_path(&["conversations", conversation_id, "load-responses"], &[])?;
        self.make_request(&path, Method::POST, Some(body), RestNamespace::AppChat)
    }

    pub fn conversation_v2_request(
        &self,
        conversation_id: &str,
        include_workspaces: bool,
        include_task_result: bool,
    ) -> Result<GrokRequest> {
        let path = endpoint_path(
            &["conversations_v2", conversation_id],
            &[
                QueryItem::new("includeWorkspaces", include_workspaces.to_string()),
                QueryItem::new("includeTaskResult", include_task_result.to_string()),
            ],
        )?;
        self.make_request(&path, Method::GET, None, RestNamespace::AppChat)
    }

    pub fn create_workspace_request(
        &self,
        options: &GrokWorkspaceCreateOptions,
    ) -> Result<GrokRequest> {
        self.make_request(
            &endpoint_path(&["workspaces"], &[])?,
            Method::POST,
            Some(json!({
                "name": options.name,
                "icon": options.icon,
                "customPersonality": options.custom_personality,
                "preferredModel": options.preferred_model
            })),
            RestNamespace::Root,
        )
    }

    pub fn list_workspaces_request(
        &self,
        options: &GrokWorkspaceListOptions,
    ) -> Result<GrokRequest> {
        let path = endpoint_path(
            &["workspaces"],
            &[
                QueryItem::new("pageSize", options.page_size.to_string()),
                QueryItem::new("orderBy", &options.order_by),
            ],
        )?;
        self.make_request(&path, Method::GET, None, RestNamespace::Root)
    }

    pub fn delete_workspace_request(&self, workspace_id: &str) -> Result<GrokRequest> {
        let path = endpoint_path(&["workspaces", workspace_id], &[])?;
        self.make_request(&path, Method::DELETE, None, RestNamespace::Root)
    }

    pub fn add_conversation_to_workspace_request(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Result<GrokRequest> {
        let path = endpoint_path(&["workspaces", workspace_id, "conversations"], &[])?;
        self.make_request(
            &path,
            Method::POST,
            Some(json!({ "conversationId": conversation_id })),
            RestNamespace::Root,
        )
    }

    pub fn speech_to_text_request(
        &self,
        audio_base64: &str,
        options: &GrokSpeechToTextOptions,
    ) -> Result<GrokRequest> {
        let trimmed_audio_base64 = audio_base64.trim();
        let trimmed_audio_format = options.audio_format.as_deref().unwrap_or_default().trim();
        let trimmed_refinement_level = options.refinement_level.trim();

        if trimmed_audio_base64.is_empty() {
            return Err(GrokError::Api("Audio input is empty".to_string()));
        }
        if trimmed_audio_format.is_empty() {
            return Err(GrokError::Api("Audio format is required".to_string()));
        }
        if trimmed_refinement_level.is_empty() {
            return Err(GrokError::Api(
                "Speech refinement level is required".to_string(),
            ));
        }

        self.make_request(
            "/voice/speech-to-text",
            Method::POST,
            Some(json!({
                "audioBase64": trimmed_audio_base64,
                "audioFormat": trimmed_audio_format,
                "refinementLevel": trimmed_refinement_level
            })),
            RestNamespace::Root,
        )
    }

    pub fn upload_file_request(
        &self,
        file_name: &str,
        file_mime_type: &str,
        content_base64: &str,
    ) -> Result<GrokRequest> {
        self.make_request(
            "/upload-file",
            Method::POST,
            Some(json!({
                "fileName": file_name,
                "fileMimeType": file_mime_type,
                "content": content_base64
            })),
            RestNamespace::AppChat,
        )
    }

    pub fn list_assets_request(&self, options: &GrokAssetListOptions) -> Result<GrokRequest> {
        let path = endpoint_path(
            &["assets"],
            &[
                QueryItem::new("pageSize", options.page_size.to_string()),
                QueryItem::new("orderBy", &options.order_by),
            ],
        )?;
        self.make_request(&path, Method::GET, None, RestNamespace::Root)
    }

    pub fn delete_asset_request(&self, asset_id: &str) -> Result<GrokRequest> {
        let path = endpoint_path(&["assets", asset_id], &[])?;
        self.make_request(&path, Method::DELETE, None, RestNamespace::Root)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::{DEFAULT_SPEECH_REFINEMENT_LEVEL, infer_audio_format};
    use crate::{
        GrokAssetListOptions, GrokClient, GrokSpeechToTextOptions, GrokWorkspaceCreateOptions,
        GrokWorkspaceListOptions, Result,
    };

    fn client() -> Result<GrokClient> {
        GrokClient::with_options(
            BTreeMap::from([("sso".to_string(), "test-cookie".to_string())]),
            false,
            Some("https://example.test/rest"),
        )
    }

    #[test]
    fn account_requests_match_swift_endpoint_contracts() -> Result<()> {
        let client = client()?;

        let typeahead = client
            .typeahead_request(
                "space / slash ? question & amp 東京",
                "en-US",
                5,
                "web app",
                2,
            )?
            .unwrap_or_else(|| panic!("typeahead request expected"));
        assert_eq!(
            typeahead.url,
            "https://example.test/_worker/typeahead?lang=en-US&maxItems=5&q=space%20/%20slash%20?%20question%20%26%20amp%20%E6%9D%B1%E4%BA%AC&platform=web%20app&source=2"
        );
        assert!(
            client
                .typeahead_request("   ", "en-US", 5, "web app", 2)?
                .is_none()
        );

        let modes = client.list_modes_request()?;
        assert_eq!(modes.method, reqwest::Method::POST);
        assert_eq!(modes.url, "https://example.test/rest/modes");
        assert_eq!(modes.body, Some(json!({})));

        let rate_limits = client.rate_limits_request("grok-420-computer-use-sa")?;
        assert_eq!(rate_limits.url, "https://example.test/rest/rate-limits");
        assert_eq!(
            rate_limits.body,
            Some(json!({"modelName": "grok-420-computer-use-sa"}))
        );
        let default_rate_limits = client.rate_limits_request("   ")?;
        assert_eq!(
            default_rate_limits.body,
            Some(json!({"modelName": crate::GrokMode::default_mode().id}))
        );
        let trimmed_rate_limits = client.rate_limits_request("  grok-4-3  ")?;
        assert_eq!(
            trimmed_rate_limits.body,
            Some(json!({"modelName": "grok-4-3"}))
        );

        let subscriptions = client.subscriptions_request()?;
        assert_eq!(subscriptions.method, reqwest::Method::GET);
        assert_eq!(subscriptions.url, "https://example.test/rest/subscriptions");
        assert!(subscriptions.body.is_none());

        let skills = client.list_skills_request("en")?;
        assert_eq!(skills.method, reqwest::Method::POST);
        assert_eq!(skills.url, "https://example.test/rest/skills");
        assert_eq!(skills.body, Some(json!({"locale": "en"})));

        let user_skills = client.list_user_skills_request()?;
        assert_eq!(user_skills.method, reqwest::Method::GET);
        assert_eq!(user_skills.url, "https://example.test/rest/user-skills");
        assert!(user_skills.body.is_none());

        let settings = client.user_settings_request()?;
        assert_eq!(settings.method, reqwest::Method::GET);
        assert_eq!(settings.url, "https://example.test/rest/user-settings");
        assert!(settings.body.is_none());

        let update_agents = client.update_agent_customizations_request(&[
            crate::GrokAgentCustomization::new(1, "Research", "Use citations"),
            crate::GrokAgentCustomization::new(0, "Custom Grok", "Base"),
        ])?;
        assert_eq!(update_agents.method, reqwest::Method::POST);
        assert_eq!(update_agents.url, "https://example.test/rest/user-settings");
        assert_eq!(
            update_agents.body,
            Some(json!({
                "agentCustomizations": {
                    "values": [
                        {
                            "agentId": 0,
                            "name": "Grok",
                            "instructions": "Base"
                        },
                        {
                            "agentId": 1,
                            "name": "Research",
                            "instructions": "Use citations"
                        }
                    ]
                }
            }))
        );

        let tasks = client.list_tasks_request()?;
        assert_eq!(tasks.method, reqwest::Method::GET);
        assert_eq!(tasks.url, "https://example.test/rest/tasks");
        assert!(tasks.body.is_none());

        let inactive_tasks = client.list_inactive_tasks_request()?;
        assert_eq!(inactive_tasks.method, reqwest::Method::GET);
        assert_eq!(
            inactive_tasks.url,
            "https://example.test/rest/tasks/inactive"
        );
        assert!(inactive_tasks.body.is_none());

        let task_results = client.task_results_request("task 123", 25)?;
        assert_eq!(task_results.method, reqwest::Method::GET);
        assert_eq!(
            task_results.url,
            "https://example.test/rest/tasks/results/task%20123?limit=25"
        );
        assert!(task_results.body.is_none());

        let create_task = client.create_task_request(
            "check command coverage",
            &crate::GrokTaskCreateOptions {
                name: "Coverage".to_string(),
                metadata_json_string: "{}".to_string(),
                schedule: Some(crate::GrokTaskSchedule::once(
                    "2026-05-15",
                    "09:30",
                    "Asia/Bangkok",
                )),
                notification_method: "DEFAULT".to_string(),
                model_mode: "BASE".to_string(),
                notification_decider_enable: true,
                notification_decider_guideline: "only notify if useful".to_string(),
                model_name: String::new(),
                toolset: vec![String::new()],
            },
        )?;
        assert_eq!(create_task.method, reqwest::Method::POST);
        assert_eq!(create_task.url, "https://example.test/rest/tasks");
        assert_eq!(
            create_task.body,
            Some(json!({
                "name": "Coverage",
                "prompt": "check command coverage",
                "metadataJsonString": "{}",
                "schedule": {
                    "taskCadence": "TASK_CADENCE_ONCE",
                    "isEnabled": true,
                    "timezone": "Asia/Bangkok",
                    "timeOfDay": "09:30",
                    "dayOfYear": "2026-05-15"
                },
                "notificationMethod": "DEFAULT",
                "modelMode": "BASE",
                "notificationDeciderEnable": true,
                "notificationDeciderGuideline": "only notify if useful",
                "modelName": "",
                "toolset": [""]
            }))
        );

        let archive_task = client.archive_task_request("task-1", false)?;
        assert_eq!(archive_task.method, reqwest::Method::PUT);
        assert_eq!(archive_task.url, "https://example.test/rest/tasks/archive");
        assert_eq!(
            archive_task.body,
            Some(json!({"taskId": "task-1", "isEnabled": false}))
        );

        let share_lookup = client.share_links_request(
            "conv space/slash?and&unicode東京",
            Some("resp space/slash?and&unicode東京"),
            &crate::GrokShareLinkOptions {
                page_size: 9,
                allow_indexing: false,
            },
        )?;
        assert_eq!(share_lookup.method, reqwest::Method::GET);
        assert_eq!(
            share_lookup.url,
            "https://example.test/rest/app-chat/share_links?pageSize=9&conversationId=conv%20space/slash?and%26unicode%E6%9D%B1%E4%BA%AC&responseId=resp%20space/slash?and%26unicode%E6%9D%B1%E4%BA%AC"
        );
        assert!(share_lookup.body.is_none());

        let share_create = client.create_share_link_request(
            "conv space/slash?and&unicode東京",
            "resp space/slash?and&unicode東京",
            &crate::GrokShareLinkOptions {
                page_size: 9,
                allow_indexing: false,
            },
        )?;
        assert_eq!(share_create.method, reqwest::Method::POST);
        assert_eq!(
            share_create.url,
            "https://example.test/rest/app-chat/conversations/conv%20space%2Fslash%3Fand%26unicode%E6%9D%B1%E4%BA%AC/share"
        );
        assert_eq!(
            share_create.body,
            Some(json!({
                "responseId": "resp space/slash?and&unicode東京",
                "allowIndexing": false
            }))
        );

        let list_conversations =
            client.list_conversations_request(&crate::GrokConversationListOptions {
                page_size: 7,
                search_query: Some("space / slash ? question & amp 東京".to_string()),
            })?;
        assert_eq!(list_conversations.method, reqwest::Method::GET);
        assert_eq!(
            list_conversations.url,
            "https://example.test/rest/app-chat/conversations?pageSize=7&searchQuery=space%20/%20slash%20?%20question%20%26%20amp%20%E6%9D%B1%E4%BA%AC"
        );
        let list_conversations_without_query =
            client.list_conversations_request(&crate::GrokConversationListOptions {
                page_size: 7,
                search_query: Some("   ".to_string()),
            })?;
        assert_eq!(
            list_conversations_without_query.url,
            "https://example.test/rest/app-chat/conversations?pageSize=7"
        );
        let list_conversations_trimmed_query =
            client.list_conversations_request(&crate::GrokConversationListOptions {
                page_size: 7,
                search_query: Some("  trimmed query  ".to_string()),
            })?;
        assert_eq!(
            list_conversations_trimmed_query.url,
            "https://example.test/rest/app-chat/conversations?pageSize=7&searchQuery=trimmed%20query"
        );

        let delete_conversation =
            client.soft_delete_conversation_request("conv space/slash?and&unicode東京")?;
        assert_eq!(delete_conversation.method, reqwest::Method::DELETE);
        assert_eq!(
            delete_conversation.url,
            "https://example.test/rest/app-chat/conversations/soft/conv%20space%2Fslash%3Fand%26unicode%E6%9D%B1%E4%BA%AC"
        );

        let response_nodes =
            client.response_nodes_request("conv space/slash?and&unicode東京", true)?;
        assert_eq!(response_nodes.method, reqwest::Method::GET);
        assert_eq!(
            response_nodes.url,
            "https://example.test/rest/app-chat/conversations/conv%20space%2Fslash%3Fand%26unicode%E6%9D%B1%E4%BA%AC/response-node?includeThreads=true"
        );

        let response_ids = vec!["resp-1".to_string(), "resp-2".to_string()];
        let load_responses =
            client.load_responses_request("conv space/slash?and&unicode東京", &response_ids)?;
        assert_eq!(load_responses.method, reqwest::Method::POST);
        assert_eq!(
            load_responses.url,
            "https://example.test/rest/app-chat/conversations/conv%20space%2Fslash%3Fand%26unicode%E6%9D%B1%E4%BA%AC/load-responses"
        );
        assert_eq!(
            load_responses.body,
            Some(json!({"responseIds": ["resp-1", "resp-2"]}))
        );

        let load_without_ids =
            client.load_responses_request("conv space/slash?and&unicode東京", &[])?;
        assert_eq!(load_without_ids.body, Some(json!({})));

        let conversation_v2 =
            client.conversation_v2_request("conv space/slash?and&unicode東京", true, true)?;
        assert_eq!(conversation_v2.method, reqwest::Method::GET);
        assert_eq!(
            conversation_v2.url,
            "https://example.test/rest/app-chat/conversations_v2/conv%20space%2Fslash%3Fand%26unicode%E6%9D%B1%E4%BA%AC?includeWorkspaces=true&includeTaskResult=true"
        );
        Ok(())
    }

    #[test]
    fn workspace_requests_match_swift_endpoint_contracts() -> Result<()> {
        let client = client()?;
        let create = client.create_workspace_request(&GrokWorkspaceCreateOptions {
            name: "Research".to_string(),
            icon: "l:book-open:lime".to_string(),
            custom_personality: "Custom instructions".to_string(),
            preferred_model: "grok-special".to_string(),
        })?;
        assert_eq!(create.url, "https://example.test/rest/workspaces");
        assert_eq!(
            create.body,
            Some(json!({
                "name": "Research",
                "icon": "l:book-open:lime",
                "customPersonality": "Custom instructions",
                "preferredModel": "grok-special"
            }))
        );

        let list = client.list_workspaces_request(&GrokWorkspaceListOptions {
            page_size: 8,
            order_by: "ORDER / ? & 東京".to_string(),
        })?;
        assert_eq!(
            list.url,
            "https://example.test/rest/workspaces?pageSize=8&orderBy=ORDER%20/%20?%20%26%20%E6%9D%B1%E4%BA%AC"
        );

        let delete = client.delete_workspace_request("workspace space/slash?and&unicode東京")?;
        assert_eq!(
            delete.url,
            "https://example.test/rest/workspaces/workspace%20space%2Fslash%3Fand%26unicode%E6%9D%B1%E4%BA%AC"
        );

        let add = client.add_conversation_to_workspace_request(
            "workspace space/slash?and&unicode東京",
            "conv space/slash?and&unicode東京",
        )?;
        assert_eq!(
            add.url,
            "https://example.test/rest/workspaces/workspace%20space%2Fslash%3Fand%26unicode%E6%9D%B1%E4%BA%AC/conversations"
        );
        assert_eq!(
            add.body,
            Some(json!({"conversationId": "conv space/slash?and&unicode東京"}))
        );
        Ok(())
    }

    #[test]
    fn files_and_audio_requests_match_swift_endpoint_contracts() -> Result<()> {
        assert_eq!(infer_audio_format("voice.webm"), Some("webm"));
        assert_eq!(infer_audio_format("VOICE.WAV"), Some("wav"));
        assert_eq!(infer_audio_format("meeting.m4a"), Some("m4a"));
        assert_eq!(infer_audio_format("voice.txt"), None);

        let client = client()?;
        let speech = client.speech_to_text_request(
            "YWJj",
            &GrokSpeechToTextOptions {
                audio_format: Some("webm".to_string()),
                refinement_level: DEFAULT_SPEECH_REFINEMENT_LEVEL.to_string(),
            },
        )?;
        assert_eq!(speech.url, "https://example.test/rest/voice/speech-to-text");
        assert_eq!(
            speech.body,
            Some(json!({
                "audioBase64": "YWJj",
                "audioFormat": "webm",
                "refinementLevel": "REFINEMENT_LEVEL_POLISH"
            }))
        );

        let upload = client.upload_file_request(
            "report space/slash?and&unicode東京.txt",
            "text/plain",
            "dGVzdA==",
        )?;
        assert_eq!(upload.url, "https://example.test/rest/app-chat/upload-file");
        assert_eq!(
            upload.body,
            Some(json!({
                "fileName": "report space/slash?and&unicode東京.txt",
                "fileMimeType": "text/plain",
                "content": "dGVzdA=="
            }))
        );

        let list = client.list_assets_request(&GrokAssetListOptions {
            page_size: 11,
            order_by: "ORDER BY / ? & 東京".to_string(),
        })?;
        assert_eq!(
            list.url,
            "https://example.test/rest/assets?pageSize=11&orderBy=ORDER%20BY%20/%20?%20%26%20%E6%9D%B1%E4%BA%AC"
        );

        let delete = client.delete_asset_request("asset space/slash?and&unicode東京")?;
        assert_eq!(
            delete.url,
            "https://example.test/rest/assets/asset%20space%2Fslash%3Fand%26unicode%E6%9D%B1%E4%BA%AC"
        );
        Ok(())
    }
}
