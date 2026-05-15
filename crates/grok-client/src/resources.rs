use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose};
use serde_json::{Map, Value};

use crate::GrokRequest;
use crate::infer_audio_format;
use crate::{
    GrokAssetListOptions, GrokClient, GrokError, GrokSpeechToTextOptions,
    GrokWorkspaceCreateOptions, GrokWorkspaceListOptions, JsonLookup, Result,
};

#[derive(Clone, Debug, PartialEq)]
pub struct GrokWorkspace {
    pub workspace_id: Option<String>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub title: Option<String>,
    pub icon: Option<String>,
    pub custom_personality: Option<String>,
    pub preferred_model: Option<String>,
    pub raw_json: Value,
}

impl GrokWorkspace {
    pub fn resolved_id(&self) -> Option<&str> {
        self.workspace_id.as_deref().or(self.id.as_deref())
    }

    pub fn display_name(&self) -> Option<&str> {
        self.name.as_deref().or(self.title.as_deref())
    }

    fn from_dictionary(dictionary: Map<String, Value>) -> Self {
        let lookup = JsonLookup::new(Value::Object(dictionary.clone()));
        Self {
            workspace_id: lookup.string(&["workspaceId", "workspace_id"]),
            id: lookup.string(&["id"]),
            name: lookup.string(&["name"]),
            title: lookup.string(&["title"]),
            icon: lookup.string(&["icon"]),
            custom_personality: lookup.string(&["customPersonality", "custom_personality"]),
            preferred_model: lookup.string(&["preferredModel", "preferred_model"]),
            raw_json: Value::Object(dictionary),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokWorkspacesResponse {
    pub workspaces: Vec<GrokWorkspace>,
    pub raw_json: Value,
}

impl GrokWorkspacesResponse {
    pub fn from_raw_json(raw_json: Value) -> Self {
        let workspaces = resource_dictionaries(
            raw_json.clone(),
            &["workspaces", "data", "result", "items"],
            &[
                "workspaceId",
                "workspace_id",
                "id",
                "name",
                "title",
                "icon",
                "customPersonality",
                "custom_personality",
                "preferredModel",
                "preferred_model",
            ],
        )
        .into_iter()
        .map(GrokWorkspace::from_dictionary)
        .collect();

        Self {
            workspaces,
            raw_json,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokWorkspaceMutationResponse {
    pub workspace: Option<GrokWorkspace>,
    pub raw_json: Value,
}

impl GrokWorkspaceMutationResponse {
    pub fn from_raw_json(raw_json: Value) -> Self {
        let workspace = first_resource_dictionary(
            &raw_json,
            &["workspace", "data", "result"],
            &[
                "workspaceId",
                "workspace_id",
                "id",
                "name",
                "title",
                "icon",
                "customPersonality",
                "custom_personality",
                "preferredModel",
                "preferred_model",
            ],
        )
        .map(GrokWorkspace::from_dictionary);

        Self {
            workspace,
            raw_json,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokSkill {
    pub skill_id: Option<String>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub title: Option<String>,
    pub status: Option<String>,
    pub description: Option<String>,
    pub raw_json: Value,
}

impl GrokSkill {
    pub fn resolved_id(&self) -> Option<&str> {
        self.skill_id.as_deref().or(self.id.as_deref())
    }

    pub fn display_name(&self) -> Option<&str> {
        self.name.as_deref().or(self.title.as_deref())
    }

    fn from_dictionary(dictionary: Map<String, Value>) -> Self {
        let lookup = JsonLookup::new(Value::Object(dictionary.clone()));
        Self {
            skill_id: lookup.string(&["skillId", "skill_id"]),
            id: lookup.string(&["id"]),
            name: lookup.string(&["name"]),
            title: lookup.string(&["title"]),
            status: lookup.string(&["status", "state"]),
            description: lookup.string(&["description", "summary"]),
            raw_json: Value::Object(dictionary),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokSkillsResponse {
    pub skills: Vec<GrokSkill>,
    pub raw_json: Value,
}

impl GrokSkillsResponse {
    pub fn from_raw_json(raw_json: Value, user_skills: bool) -> Self {
        let container_keys: &[&str] = if user_skills {
            &["userSkills", "skills", "data", "result", "items"]
        } else {
            &["skills", "data", "result", "items"]
        };
        let skills = resource_dictionaries(
            raw_json.clone(),
            container_keys,
            &[
                "skillId",
                "skill_id",
                "id",
                "name",
                "title",
                "displayName",
                "description",
                "summary",
                "status",
                "state",
            ],
        )
        .into_iter()
        .map(GrokSkill::from_dictionary)
        .collect();

        Self { skills, raw_json }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrokAgentCustomization {
    pub agent_id: i64,
    pub name: String,
    pub instructions: String,
}

impl GrokAgentCustomization {
    pub fn new(agent_id: i64, name: impl Into<String>, instructions: impl Into<String>) -> Self {
        let name = if agent_id == 0 {
            "Grok".to_string()
        } else {
            name.into()
        };
        Self {
            agent_id,
            name,
            instructions: instructions.into(),
        }
    }

    pub fn default_name(agent_id: i64) -> String {
        match agent_id {
            0 => "Grok".to_string(),
            1 => "Grok II".to_string(),
            2 => "Grok III".to_string(),
            3 => "Grok IV".to_string(),
            _ => format!("Agent {agent_id}"),
        }
    }

    fn from_dictionary(dictionary: Map<String, Value>) -> Option<Self> {
        let lookup = JsonLookup::new(Value::Object(dictionary.clone()));
        let nested_agent = dictionary
            .get("agent")
            .and_then(Value::as_object)
            .cloned()
            .map(Value::Object)
            .map(JsonLookup::new);
        let agent_id = lookup.int(&["agentId", "agent_id", "id"])?;
        let name = lookup
            .string(&["name"])
            .or_else(|| {
                nested_agent
                    .as_ref()
                    .and_then(|lookup| lookup.string(&["name"]))
            })
            .unwrap_or_else(|| Self::default_name(agent_id));
        let instructions = lookup
            .string_allowing_empty(&["instructions", "customInstructions", "custom_instructions"])
            .or_else(|| {
                nested_agent.as_ref().and_then(|lookup| {
                    lookup.string_allowing_empty(&[
                        "instructions",
                        "customInstructions",
                        "custom_instructions",
                    ])
                })
            })
            .unwrap_or_default();
        Some(Self::new(agent_id, name, instructions))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokAgentCustomizationsResponse {
    pub agent_customizations: Vec<GrokAgentCustomization>,
    pub raw_json: Value,
}

impl GrokAgentCustomizationsResponse {
    pub fn from_raw_json(raw_json: Value) -> Self {
        let mut agent_customizations = agent_customization_dictionaries(&raw_json)
            .into_iter()
            .filter_map(GrokAgentCustomization::from_dictionary)
            .collect::<Vec<_>>();
        agent_customizations.sort_by_key(|agent| agent.agent_id);
        Self {
            agent_customizations,
            raw_json,
        }
    }
}

fn agent_customization_dictionaries(value: &Value) -> Vec<Map<String, Value>> {
    let dictionaries = JsonLookup::new(value.clone()).dictionaries(&[
        "values",
        "agentCustomizations",
        "agent_customizations",
        "userSettings",
        "user_settings",
        "settings",
        "data",
        "result",
        "items",
    ]);
    if dictionaries.iter().any(has_agent_customization_identity) {
        return dictionaries;
    }

    dictionaries
        .into_iter()
        .flat_map(|dictionary| agent_customization_dictionaries(&Value::Object(dictionary)))
        .collect()
}

fn has_agent_customization_identity(dictionary: &Map<String, Value>) -> bool {
    dictionary.contains_key("agentId")
        || dictionary.contains_key("agent_id")
        || dictionary.contains_key("id")
        || dictionary.contains_key("agent")
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokAsset {
    pub asset_id: Option<String>,
    pub file_metadata_id: Option<String>,
    pub file_id: Option<String>,
    pub id: Option<String>,
    pub file_name: Option<String>,
    pub name: Option<String>,
    pub mime_type: Option<String>,
    pub raw_json: Value,
}

impl GrokAsset {
    pub fn resolved_id(&self) -> Option<&str> {
        self.file_metadata_id
            .as_deref()
            .or(self.file_id.as_deref())
            .or(self.asset_id.as_deref())
            .or(self.id.as_deref())
    }

    pub fn display_file_name(&self) -> Option<&str> {
        self.file_name.as_deref().or(self.name.as_deref())
    }

    fn from_dictionary(dictionary: Map<String, Value>) -> Self {
        let lookup = JsonLookup::new(Value::Object(dictionary.clone()));
        Self {
            asset_id: lookup.string(&["assetId", "asset_id"]),
            file_metadata_id: lookup.string(&["fileMetadataId", "file_metadata_id"]),
            file_id: lookup.string(&["fileId", "file_id"]),
            id: lookup.string(&["id"]),
            file_name: lookup.string(&["fileName", "file_name"]),
            name: lookup.string(&["name"]),
            mime_type: lookup.string(&["mimeType", "mime_type", "fileMimeType"]),
            raw_json: Value::Object(dictionary),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokAssetsResponse {
    pub assets: Vec<GrokAsset>,
    pub raw_json: Value,
}

impl GrokAssetsResponse {
    pub fn from_raw_json(raw_json: Value) -> Self {
        let assets = resource_dictionaries(
            raw_json.clone(),
            &["assets", "data", "result", "items"],
            &[
                "assetId",
                "asset_id",
                "fileMetadataId",
                "file_metadata_id",
                "fileId",
                "file_id",
                "id",
                "fileName",
                "file_name",
                "name",
                "mimeType",
                "mime_type",
                "fileMimeType",
            ],
        )
        .into_iter()
        .map(GrokAsset::from_dictionary)
        .collect();

        Self { assets, raw_json }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokAssetMutationResponse {
    pub asset: Option<GrokAsset>,
    pub raw_json: Value,
}

impl GrokAssetMutationResponse {
    pub fn from_raw_json(raw_json: Value) -> Self {
        let asset = first_resource_dictionary(
            &raw_json,
            &["asset", "file", "data", "result"],
            &[
                "assetId",
                "asset_id",
                "fileMetadataId",
                "file_metadata_id",
                "fileId",
                "file_id",
                "id",
                "fileName",
                "file_name",
                "name",
                "mimeType",
                "mime_type",
                "fileMimeType",
            ],
        )
        .map(GrokAsset::from_dictionary);

        Self { asset, raw_json }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokFileUploadResponse {
    pub file_metadata_id: Option<String>,
    pub file_id: Option<String>,
    pub asset_id: Option<String>,
    pub id: Option<String>,
    pub file_name: Option<String>,
    pub asset: Option<GrokAsset>,
    pub raw_json: Value,
}

impl GrokFileUploadResponse {
    pub fn uploaded_file_id(&self) -> Option<&str> {
        self.file_metadata_id
            .as_deref()
            .or(self.file_id.as_deref())
            .or(self.asset_id.as_deref())
            .or(self.id.as_deref())
            .or_else(|| self.asset.as_ref().and_then(GrokAsset::resolved_id))
    }

    pub fn from_raw_json(raw_json: Value) -> Self {
        let lookup = JsonLookup::new(raw_json.clone());
        let asset = first_resource_dictionary(
            &raw_json,
            &["asset", "file", "data", "result"],
            &[
                "assetId",
                "asset_id",
                "fileMetadataId",
                "file_metadata_id",
                "fileId",
                "file_id",
                "id",
                "fileName",
                "file_name",
                "name",
                "mimeType",
                "mime_type",
                "fileMimeType",
            ],
        )
        .map(GrokAsset::from_dictionary);

        Self {
            file_metadata_id: lookup.first_string(&["fileMetadataId", "file_metadata_id"]),
            file_id: lookup.first_string(&["fileId", "file_id"]),
            asset_id: lookup.first_string(&["assetId", "asset_id"]),
            id: lookup.first_string(&["id"]),
            file_name: lookup.first_string(&["fileName", "file_name", "name"]),
            asset,
            raw_json,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokSpeechToTextResponse {
    pub text: String,
    pub raw_json: Value,
}

impl GrokSpeechToTextResponse {
    pub fn from_raw_json(raw_json: Value) -> Result<Self> {
        let text = JsonLookup::new(raw_json.clone())
            .first_string(&["text", "transcript", "message"])
            .ok_or_else(|| {
                GrokError::Api("Speech-to-text response did not include a transcript".to_string())
            })?;

        Ok(Self { text, raw_json })
    }
}

fn resource_dictionaries(
    raw_json: Value,
    container_keys: &[&str],
    field_markers: &[&str],
) -> Vec<Map<String, Value>> {
    let dictionaries = JsonLookup::new(raw_json).dictionaries(container_keys);
    if dictionaries.len() != 1 || contains_any_key(&dictionaries[0], field_markers) {
        return dictionaries;
    }

    let nested =
        JsonLookup::new(Value::Object(dictionaries[0].clone())).dictionaries(container_keys);
    if nested.is_empty() {
        dictionaries
    } else {
        nested
    }
}

fn contains_any_key(dictionary: &Map<String, Value>, keys: &[&str]) -> bool {
    keys.iter().any(|key| dictionary.contains_key(*key))
}

fn first_resource_dictionary(
    raw_json: &Value,
    container_keys: &[&str],
    field_markers: &[&str],
) -> Option<Map<String, Value>> {
    let lookup = JsonLookup::new(raw_json.clone());
    if let Some(dictionary) = lookup.first_dictionary(container_keys)
        && contains_any_key(&dictionary, field_markers)
    {
        return Some(dictionary);
    }

    raw_json
        .as_object()
        .filter(|dictionary| contains_any_key(dictionary, field_markers))
        .cloned()
}

impl GrokClient {
    pub async fn create_workspace_response(
        &self,
        options: &GrokWorkspaceCreateOptions,
    ) -> Result<GrokWorkspaceMutationResponse> {
        let request = self.create_workspace_request(options)?;
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokWorkspaceMutationResponse::from_raw_json(raw_json))
    }

    pub async fn list_workspaces_response(
        &self,
        options: &GrokWorkspaceListOptions,
    ) -> Result<GrokWorkspacesResponse> {
        let request = self.list_workspaces_request(options)?;
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokWorkspacesResponse::from_raw_json(raw_json))
    }

    pub async fn delete_workspace(
        &self,
        workspace_id: &str,
    ) -> Result<GrokWorkspaceMutationResponse> {
        let request = self.delete_workspace_request(workspace_id)?;
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokWorkspaceMutationResponse::from_raw_json(raw_json))
    }

    pub async fn add_conversation_to_workspace(
        &self,
        workspace_id: &str,
        conversation_id: &str,
    ) -> Result<GrokWorkspaceMutationResponse> {
        let request = self.add_conversation_to_workspace_request(workspace_id, conversation_id)?;
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokWorkspaceMutationResponse::from_raw_json(raw_json))
    }

    pub async fn list_skills_response(&self, locale: &str) -> Result<GrokSkillsResponse> {
        let request = self.list_skills_request(locale)?;
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokSkillsResponse::from_raw_json(raw_json, false))
    }

    pub async fn list_skills(&self, locale: &str) -> Result<Vec<GrokSkill>> {
        Ok(self.list_skills_response(locale).await?.skills)
    }

    pub async fn list_user_skills_response(&self) -> Result<GrokSkillsResponse> {
        let request = self.list_user_skills_request()?;
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokSkillsResponse::from_raw_json(raw_json, true))
    }

    pub async fn list_user_skills(&self) -> Result<Vec<GrokSkill>> {
        Ok(self.list_user_skills_response().await?.skills)
    }

    pub async fn get_user_settings_response(&self) -> Result<GrokAgentCustomizationsResponse> {
        let request = self.user_settings_request()?;
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokAgentCustomizationsResponse::from_raw_json(raw_json))
    }

    pub async fn update_agent_customizations(
        &self,
        customizations: &[GrokAgentCustomization],
    ) -> Result<GrokAgentCustomizationsResponse> {
        let request = self.update_agent_customizations_request(customizations)?;
        let raw_json = self.send_request_json(request).await?;
        let response = GrokAgentCustomizationsResponse::from_raw_json(raw_json.clone());
        if response.agent_customizations.is_empty() {
            Ok(GrokAgentCustomizationsResponse {
                agent_customizations: customizations.to_vec(),
                raw_json,
            })
        } else {
            Ok(response)
        }
    }

    pub async fn list_assets_response(
        &self,
        options: &GrokAssetListOptions,
    ) -> Result<GrokAssetsResponse> {
        let request = self.list_assets_request(options)?;
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokAssetsResponse::from_raw_json(raw_json))
    }

    pub async fn delete_asset(&self, asset_id: &str) -> Result<GrokAssetMutationResponse> {
        let request = self.delete_asset_request(asset_id)?;
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokAssetMutationResponse::from_raw_json(raw_json))
    }

    pub async fn upload_file_response(
        &self,
        file_name: &str,
        file_mime_type: &str,
        content_base64: &str,
    ) -> Result<GrokFileUploadResponse> {
        let request = self.upload_file_request(file_name, file_mime_type, content_base64)?;
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokFileUploadResponse::from_raw_json(raw_json))
    }

    pub fn upload_file_path_request(
        &self,
        path: impl AsRef<Path>,
        mime_type: Option<&str>,
    ) -> Result<GrokRequest> {
        let expanded_path = expand_tilde_path(path.as_ref());
        let data = std::fs::read(&expanded_path).map_err(|error| {
            GrokError::Api(format!(
                "Could not read upload file {}: {error}",
                expanded_path.display()
            ))
        })?;
        let file_name = file_name_for_path(&expanded_path)?;
        let resolved_mime_type = mime_type.unwrap_or("application/octet-stream");
        let content_base64 = general_purpose::STANDARD.encode(data);
        self.upload_file_request(&file_name, resolved_mime_type, &content_base64)
    }

    pub async fn upload_file_path_response(
        &self,
        path: impl AsRef<Path>,
        mime_type: Option<&str>,
    ) -> Result<GrokFileUploadResponse> {
        let request = self.upload_file_path_request(path, mime_type)?;
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokFileUploadResponse::from_raw_json(raw_json))
    }

    pub async fn speech_to_text_response(
        &self,
        audio_base64: &str,
        options: &GrokSpeechToTextOptions,
    ) -> Result<GrokSpeechToTextResponse> {
        let request = self.speech_to_text_request(audio_base64, options)?;
        let raw_json = self.send_request_json(request).await?;
        GrokSpeechToTextResponse::from_raw_json(raw_json)
    }

    pub fn speech_to_text_file_request(
        &self,
        path: impl AsRef<Path>,
        options: &GrokSpeechToTextOptions,
    ) -> Result<GrokRequest> {
        let expanded_path = expand_tilde_path(path.as_ref());
        let data = std::fs::read(&expanded_path).map_err(|error| {
            GrokError::Api(format!(
                "Could not read audio file {}: {error}",
                expanded_path.display()
            ))
        })?;
        let file_name = file_name_for_path(&expanded_path)?;
        let resolved_audio_format = match options.audio_format.as_deref() {
            Some(audio_format) => Some(audio_format.to_string()),
            None => infer_audio_format(&file_name).map(str::to_string),
        }
        .ok_or_else(|| {
            GrokError::Api(format!(
                "Could not infer audio format for {file_name}. Pass an explicit audio format."
            ))
        })?;
        let audio_base64 = general_purpose::STANDARD.encode(data);
        self.speech_to_text_request(
            &audio_base64,
            &GrokSpeechToTextOptions {
                audio_format: Some(resolved_audio_format),
                refinement_level: options.refinement_level.clone(),
            },
        )
    }

    pub async fn speech_to_text_file_response(
        &self,
        path: impl AsRef<Path>,
        options: &GrokSpeechToTextOptions,
    ) -> Result<GrokSpeechToTextResponse> {
        let request = self.speech_to_text_file_request(path, options)?;
        let raw_json = self.send_request_json(request).await?;
        GrokSpeechToTextResponse::from_raw_json(raw_json)
    }
}

fn file_name_for_path(path: &Path) -> Result<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| GrokError::Api(format!("Path has no file name: {}", path.display())))
}

fn expand_tilde_path(path: &Path) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    expand_tilde_path_with_home(path, home.as_deref())
}

fn expand_tilde_path_with_home(path: &Path, home: Option<&Path>) -> PathBuf {
    let raw_path = path.to_string_lossy();
    let Some(home) = home else {
        return path.to_path_buf();
    };

    if raw_path == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = raw_path.strip_prefix("~/") {
        return home.join(rest);
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use serde_json::json;

    use crate::{DEFAULT_SPEECH_REFINEMENT_LEVEL, GrokClient, GrokSpeechToTextOptions};

    use super::{
        GrokAgentCustomizationsResponse, GrokAssetMutationResponse, GrokAssetsResponse,
        GrokFileUploadResponse, GrokSkillsResponse, GrokSpeechToTextResponse,
        GrokWorkspaceMutationResponse, GrokWorkspacesResponse, expand_tilde_path_with_home,
    };

    fn client() -> crate::Result<GrokClient> {
        GrokClient::with_options(
            BTreeMap::from([("sso".to_string(), "cookie".to_string())]),
            false,
            Some("https://example.test/rest"),
        )
    }

    #[test]
    fn parses_assets_with_swift_field_aliases() {
        let response = GrokAssetsResponse::from_raw_json(json!({
            "data": {
                "items": [
                    {
                        "file_metadata_id": "meta-1",
                        "file_name": "report.pdf",
                        "fileMimeType": "application/pdf"
                    },
                    {
                        "assetId": "asset-2",
                        "name": "photo.jpg",
                        "mime_type": "image/jpeg"
                    }
                ]
            }
        }));

        assert_eq!(response.assets.len(), 2);
        assert_eq!(response.assets[0].resolved_id(), Some("meta-1"));
        assert_eq!(response.assets[0].display_file_name(), Some("report.pdf"));
        assert_eq!(
            response.assets[0].mime_type.as_deref(),
            Some("application/pdf")
        );
        assert_eq!(response.assets[1].resolved_id(), Some("asset-2"));
        assert_eq!(response.assets[1].display_file_name(), Some("photo.jpg"));
    }

    #[test]
    fn parses_workspaces_with_swift_field_aliases() {
        let response = GrokWorkspacesResponse::from_raw_json(json!({
            "result": {
                "workspaces": [
                    {
                        "workspace_id": "workspace-1",
                        "title": "Research",
                        "preferred_model": "grok-special",
                        "custom_personality": "Use citations"
                    },
                    {
                        "id": "workspace-2",
                        "name": "Planning",
                        "icon": "l:book-open:lime"
                    }
                ]
            }
        }));

        assert_eq!(response.workspaces.len(), 2);
        assert_eq!(response.workspaces[0].resolved_id(), Some("workspace-1"));
        assert_eq!(response.workspaces[0].display_name(), Some("Research"));
        assert_eq!(
            response.workspaces[0].preferred_model.as_deref(),
            Some("grok-special")
        );
        assert_eq!(response.workspaces[1].resolved_id(), Some("workspace-2"));
        assert_eq!(response.workspaces[1].display_name(), Some("Planning"));
    }

    #[test]
    fn parses_skills_with_swift_field_aliases() {
        let response = GrokSkillsResponse::from_raw_json(
            json!({
                "data": {
                    "items": [
                        {
                            "skill_id": "skill-1",
                            "name": "Research",
                            "summary": "Finds things",
                            "state": "active"
                        },
                        {
                            "id": "skill-2",
                            "title": "Drafting",
                            "description": "Writes things"
                        }
                    ]
                }
            }),
            false,
        );

        assert_eq!(response.skills.len(), 2);
        assert_eq!(response.skills[0].resolved_id(), Some("skill-1"));
        assert_eq!(response.skills[0].display_name(), Some("Research"));
        assert_eq!(
            response.skills[0].description.as_deref(),
            Some("Finds things")
        );
        assert_eq!(response.skills[0].status.as_deref(), Some("active"));
        assert_eq!(response.skills[1].resolved_id(), Some("skill-2"));
        assert_eq!(response.skills[1].display_name(), Some("Drafting"));
    }

    #[test]
    fn parses_skill_status_and_description_aliases() {
        let response = GrokSkillsResponse::from_raw_json(
            json!({
                "skills": [
                    {
                        "id": "skill-state-summary",
                        "name": "Research",
                        "state": "enabled",
                        "summary": "Summary alias"
                    },
                    {
                        "id": "skill-status-description",
                        "title": "Drafting",
                        "status": "installed",
                        "description": "Description alias"
                    }
                ]
            }),
            false,
        );

        assert_eq!(response.skills.len(), 2);
        assert_eq!(
            response.skills[0].description.as_deref(),
            Some("Summary alias")
        );
        assert_eq!(response.skills[0].status.as_deref(), Some("enabled"));
        assert_eq!(
            response.skills[1].description.as_deref(),
            Some("Description alias")
        );
        assert_eq!(response.skills[1].status.as_deref(), Some("installed"));
    }

    #[test]
    fn parses_agent_customizations_with_swift_field_aliases() {
        let response = GrokAgentCustomizationsResponse::from_raw_json(json!({
            "userSettings": {
                "agentCustomizations": {
                    "values": [
                        {
                            "agentId": 0,
                            "name": "Ignored",
                            "instructions": "base instructions"
                        },
                        {
                            "agent_id": 2,
                            "name": "Research",
                            "custom_instructions": "Use citations"
                        }
                    ]
                }
            }
        }));

        assert_eq!(response.agent_customizations.len(), 2);
        assert_eq!(response.agent_customizations[0].agent_id, 0);
        assert_eq!(response.agent_customizations[0].name, "Grok");
        assert_eq!(
            response.agent_customizations[0].instructions,
            "base instructions"
        );
        assert_eq!(response.agent_customizations[1].agent_id, 2);
        assert_eq!(response.agent_customizations[1].name, "Research");
        assert_eq!(
            response.agent_customizations[1].instructions,
            "Use citations"
        );
    }

    #[test]
    fn parses_workspace_mutation_response_from_common_wrappers() {
        let response = GrokWorkspaceMutationResponse::from_raw_json(json!({
            "workspace": {
                "workspaceId": "workspace-1",
                "name": "Research",
                "preferredModel": "grok-special"
            }
        }));

        let Some(workspace) = response.workspace.as_ref() else {
            panic!("workspace should parse");
        };
        assert_eq!(workspace.resolved_id(), Some("workspace-1"));
        assert_eq!(workspace.display_name(), Some("Research"));
        assert_eq!(workspace.preferred_model.as_deref(), Some("grok-special"));
    }

    #[test]
    fn parses_asset_mutation_response_from_common_wrappers() {
        let response = GrokAssetMutationResponse::from_raw_json(json!({
            "data": {
                "file_metadata_id": "meta-1",
                "file_name": "report.pdf",
                "fileMimeType": "application/pdf"
            }
        }));

        let Some(asset) = response.asset.as_ref() else {
            panic!("asset should parse");
        };
        assert_eq!(asset.resolved_id(), Some("meta-1"));
        assert_eq!(asset.display_file_name(), Some("report.pdf"));
    }

    #[test]
    fn empty_asset_mutation_response_has_no_item() {
        let response = GrokAssetMutationResponse::from_raw_json(json!({}));

        assert!(response.asset.is_none());
    }

    #[test]
    fn parses_file_upload_response_like_swift_upload_json() {
        let response = GrokFileUploadResponse::from_raw_json(json!({
            "fileMetadataId": "meta-1",
            "fileName": "report.pdf",
            "asset": {
                "assetId": "asset-1",
                "fileName": "report.pdf",
                "mimeType": "application/pdf"
            }
        }));

        assert_eq!(response.uploaded_file_id(), Some("meta-1"));
        assert_eq!(response.file_name.as_deref(), Some("report.pdf"));
        let Some(asset) = response.asset.as_ref() else {
            panic!("asset should parse");
        };
        assert_eq!(asset.resolved_id(), Some("asset-1"));
    }

    #[test]
    fn upload_response_resolves_id_from_nested_asset() {
        let response = GrokFileUploadResponse::from_raw_json(json!({
            "asset": {
                "fileMetadataId": "meta-nested",
                "fileName": "nested.txt"
            }
        }));

        assert_eq!(response.uploaded_file_id(), Some("meta-nested"));
        assert_eq!(response.file_name.as_deref(), Some("nested.txt"));
    }

    #[test]
    fn parses_speech_to_text_response_variants() -> crate::Result<()> {
        let variants = [
            (json!({"transcript": "top transcript"}), "top transcript"),
            (
                json!({"data": {"text": "nested data text"}}),
                "nested data text",
            ),
            (
                json!({"result": {"text": "nested result text"}}),
                "nested result text",
            ),
            (
                json!({"choices": [{"message": "array message"}]}),
                "array message",
            ),
            (
                json!({"result": {"transcript": "hello audio"}}),
                "hello audio",
            ),
        ];

        for (raw_json, expected_text) in variants {
            let response = GrokSpeechToTextResponse::from_raw_json(raw_json)?;
            assert_eq!(response.text, expected_text);
        }

        let error = match GrokSpeechToTextResponse::from_raw_json(json!({"ok": true})) {
            Ok(response) => panic!("missing transcript should fail, got {response:?}"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("Speech-to-text response did not include a transcript")
        );
        Ok(())
    }

    #[test]
    fn speech_to_text_file_request_reads_path_and_infers_format_like_swift()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let audio_file = temp_dir.path().join("clip.wav");
        std::fs::write(&audio_file, "wav bytes")?;
        let client = client()?;

        let request = client.speech_to_text_file_request(
            &audio_file,
            &GrokSpeechToTextOptions {
                audio_format: None,
                refinement_level: DEFAULT_SPEECH_REFINEMENT_LEVEL.to_string(),
            },
        )?;

        assert_eq!(
            request.url,
            "https://example.test/rest/voice/speech-to-text"
        );
        assert_eq!(
            request.body,
            Some(json!({
                "audioBase64": "d2F2IGJ5dGVz",
                "audioFormat": "wav",
                "refinementLevel": "REFINEMENT_LEVEL_POLISH"
            }))
        );

        let unknown_file = temp_dir.path().join("clip.bin");
        std::fs::write(&unknown_file, "audio")?;
        let error = match client.speech_to_text_file_request(
            &unknown_file,
            &GrokSpeechToTextOptions {
                audio_format: None,
                refinement_level: DEFAULT_SPEECH_REFINEMENT_LEVEL.to_string(),
            },
        ) {
            Ok(request) => panic!("unknown extension should fail, got {request:?}"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("Could not infer audio format"));
        Ok(())
    }

    #[test]
    fn speech_to_text_file_request_honors_explicit_format_like_swift()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let audio_file = temp_dir.path().join("clip.audio");
        std::fs::write(&audio_file, "wav bytes")?;
        let client = client()?;

        let request = client.speech_to_text_file_request(
            &audio_file,
            &GrokSpeechToTextOptions {
                audio_format: Some("wav".to_string()),
                refinement_level: "REFINEMENT_LEVEL_RAW".to_string(),
            },
        )?;

        assert_eq!(
            request.body,
            Some(json!({
                "audioBase64": "d2F2IGJ5dGVz",
                "audioFormat": "wav",
                "refinementLevel": "REFINEMENT_LEVEL_RAW"
            }))
        );
        Ok(())
    }

    #[test]
    fn upload_file_path_request_reads_basename_and_mime_like_swift()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let upload_file = temp_dir.path().join("report.txt");
        std::fs::write(&upload_file, "upload bytes")?;
        let client = client()?;

        let default_mime_request = client.upload_file_path_request(&upload_file, None)?;
        assert_eq!(
            default_mime_request.url,
            "https://example.test/rest/app-chat/upload-file"
        );
        assert_eq!(
            default_mime_request.body,
            Some(json!({
                "fileName": "report.txt",
                "fileMimeType": "application/octet-stream",
                "content": "dXBsb2FkIGJ5dGVz"
            }))
        );

        let explicit_mime_request =
            client.upload_file_path_request(&upload_file, Some("text/plain"))?;
        assert_eq!(
            explicit_mime_request.body,
            Some(json!({
                "fileName": "report.txt",
                "fileMimeType": "text/plain",
                "content": "dXBsb2FkIGJ5dGVz"
            }))
        );
        Ok(())
    }

    #[test]
    fn expands_tilde_paths_like_swift() {
        let home = Path::new("/tmp/grok-home");

        assert_eq!(
            expand_tilde_path_with_home(Path::new("~/voice.wav"), Some(home)),
            home.join("voice.wav")
        );
        assert_eq!(
            expand_tilde_path_with_home(Path::new("/tmp/voice.wav"), Some(home)),
            Path::new("/tmp/voice.wav")
        );
        assert_eq!(
            expand_tilde_path_with_home(Path::new("~/voice.wav"), None),
            Path::new("~/voice.wav")
        );
    }
}
