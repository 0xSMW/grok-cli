use grok_client::{GrokAsset, GrokWorkspace};

pub fn workspace_resolved_id(workspace: &GrokWorkspace) -> Option<&str> {
    workspace.resolved_id()
}

pub fn workspace_display_name(workspace: &GrokWorkspace) -> String {
    workspace
        .display_name()
        .or_else(|| workspace_resolved_id(workspace))
        .unwrap_or("Untitled workspace")
        .to_string()
}

pub fn asset_display_name(asset: &GrokAsset) -> String {
    asset
        .display_file_name()
        .or_else(|| asset.resolved_id())
        .unwrap_or("Untitled file")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{asset_display_name, workspace_display_name, workspace_resolved_id};
    use grok_client::{GrokAsset, GrokWorkspace};
    use serde_json::Value;

    #[test]
    fn workspace_display_name_matches_swift_cli_extension_order() {
        assert_eq!(
            workspace_display_name(&workspace(
                Some("workspace-1"),
                Some("id-1"),
                Some("Research"),
                Some("Planning")
            )),
            "Research"
        );
        assert_eq!(
            workspace_display_name(&workspace(
                Some("workspace-1"),
                Some("id-1"),
                None,
                Some("Planning")
            )),
            "Planning"
        );
        assert_eq!(
            workspace_display_name(&workspace(Some("workspace-1"), Some("id-1"), None, None)),
            "workspace-1"
        );
        assert_eq!(
            workspace_display_name(&workspace(None, Some("id-1"), None, None)),
            "id-1"
        );
        assert_eq!(
            workspace_display_name(&workspace(None, None, None, None)),
            "Untitled workspace"
        );
    }

    #[test]
    fn workspace_resolved_id_matches_swift_cli_extension_order() {
        assert_eq!(
            workspace_resolved_id(&workspace(Some("workspace-1"), Some("id-1"), None, None)),
            Some("workspace-1")
        );
        assert_eq!(
            workspace_resolved_id(&workspace(None, Some("id-1"), None, None)),
            Some("id-1")
        );
        assert_eq!(
            workspace_resolved_id(&workspace(None, None, None, None)),
            None
        );
    }

    #[test]
    fn asset_display_name_matches_swift_cli_extension_order() {
        assert_eq!(
            asset_display_name(&asset(Some("report.pdf"), Some("Report"), Some("file-1"))),
            "report.pdf"
        );
        assert_eq!(
            asset_display_name(&asset(None, Some("Report"), Some("file-1"))),
            "Report"
        );
        assert_eq!(
            asset_display_name(&asset(None, None, Some("file-1"))),
            "file-1"
        );
        assert_eq!(
            asset_display_name(&asset(None, None, None)),
            "Untitled file"
        );
    }

    fn workspace(
        workspace_id: Option<&str>,
        id: Option<&str>,
        name: Option<&str>,
        title: Option<&str>,
    ) -> GrokWorkspace {
        GrokWorkspace {
            workspace_id: workspace_id.map(str::to_string),
            id: id.map(str::to_string),
            name: name.map(str::to_string),
            title: title.map(str::to_string),
            icon: None,
            custom_personality: None,
            preferred_model: None,
            raw_json: Value::Null,
        }
    }

    fn asset(file_name: Option<&str>, name: Option<&str>, id: Option<&str>) -> GrokAsset {
        GrokAsset {
            asset_id: None,
            file_metadata_id: id.map(str::to_string),
            file_id: None,
            id: None,
            file_name: file_name.map(str::to_string),
            name: name.map(str::to_string),
            mime_type: None,
            raw_json: Value::Null,
        }
    }
}
