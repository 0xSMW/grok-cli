use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::{DEFAULT_SPEECH_REFINEMENT_LEVEL, GrokMode};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum GrokPersonalityType {
    Romance,
    MedicalAdvisor,
    LatestNews,
    UnhingedComedian,
    LoyalFriend,
    HomeworkHelper,
    TrustedTherapist,
    #[default]
    None,
}

impl GrokPersonalityType {
    pub const ALL: [Self; 8] = [
        Self::Romance,
        Self::MedicalAdvisor,
        Self::LatestNews,
        Self::UnhingedComedian,
        Self::LoyalFriend,
        Self::HomeworkHelper,
        Self::TrustedTherapist,
        Self::None,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Romance => "grok3_personality_romance_me",
            Self::MedicalAdvisor => "grok3_personality_medical_advisor",
            Self::LatestNews => "grok3_personality_latest_news",
            Self::UnhingedComedian => "grok3_personality_unhinged_comedian",
            Self::LoyalFriend => "grok3_personality_loyal_friend",
            Self::HomeworkHelper => "grok3_personality_homework_helper",
            Self::TrustedTherapist => "grok3_personality_trusted_therapist",
            Self::None => "",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Romance => "Romance Me",
            Self::MedicalAdvisor => "Medical Advisor",
            Self::LatestNews => "Latest News",
            Self::UnhingedComedian => "Unhinged Comedian",
            Self::LoyalFriend => "Loyal Friend",
            Self::HomeworkHelper => "Homework Helper",
            Self::TrustedTherapist => "Trusted Therapist",
            Self::None => "Default (No Personality)",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Romance => "A flirty and romantic personality",
            Self::MedicalAdvisor => "A helpful medical information advisor",
            Self::LatestNews => "Focused on providing the latest news and current events",
            Self::UnhingedComedian => "A wild and unhinged comedian",
            Self::LoyalFriend => "A supportive and loyal friend",
            Self::HomeworkHelper => "A patient tutor focused on helping with homework",
            Self::TrustedTherapist => "A compassionate therapeutic personality",
            Self::None => "Standard Grok personality",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "grok3_personality_romance_me" => Some(Self::Romance),
            "grok3_personality_medical_advisor" => Some(Self::MedicalAdvisor),
            "grok3_personality_latest_news" => Some(Self::LatestNews),
            "grok3_personality_unhinged_comedian" => Some(Self::UnhingedComedian),
            "grok3_personality_loyal_friend" => Some(Self::LoyalFriend),
            "grok3_personality_homework_helper" => Some(Self::HomeworkHelper),
            "grok3_personality_trusted_therapist" => Some(Self::TrustedTherapist),
            "" => Some(Self::None),
            _ => None,
        }
    }
}

impl Serialize for GrokPersonalityType {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.id())
    }
}

impl<'de> Deserialize<'de> for GrokPersonalityType {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let id = String::deserialize(deserializer)?;
        Self::from_id(&id)
            .ok_or_else(|| de::Error::custom(format!("unknown Grok personality type id: {id}")))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokMessageOptions {
    pub enable_reasoning: bool,
    pub enable_deep_search: bool,
    pub disable_search: bool,
    pub custom_instructions: String,
    pub temporary: bool,
    pub personality_type: GrokPersonalityType,
    pub mode_id: String,
    pub file_attachments: Vec<String>,
    pub workspace_ids: Vec<String>,
    pub disabled_connector_ids: Vec<String>,
}

impl Default for GrokMessageOptions {
    fn default() -> Self {
        Self {
            enable_reasoning: true,
            enable_deep_search: false,
            disable_search: false,
            custom_instructions: String::new(),
            temporary: false,
            personality_type: GrokPersonalityType::None,
            mode_id: GrokMode::default_mode().id,
            file_attachments: Vec::new(),
            workspace_ids: Vec::new(),
            disabled_connector_ids: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GrokMessageOptions, GrokPersonalityType};
    use crate::Result;

    #[test]
    fn personality_type_matches_swift_raw_values_display_names_and_descriptions() {
        let cases = [
            (
                GrokPersonalityType::Romance,
                "grok3_personality_romance_me",
                "Romance Me",
                "A flirty and romantic personality",
            ),
            (
                GrokPersonalityType::MedicalAdvisor,
                "grok3_personality_medical_advisor",
                "Medical Advisor",
                "A helpful medical information advisor",
            ),
            (
                GrokPersonalityType::LatestNews,
                "grok3_personality_latest_news",
                "Latest News",
                "Focused on providing the latest news and current events",
            ),
            (
                GrokPersonalityType::UnhingedComedian,
                "grok3_personality_unhinged_comedian",
                "Unhinged Comedian",
                "A wild and unhinged comedian",
            ),
            (
                GrokPersonalityType::LoyalFriend,
                "grok3_personality_loyal_friend",
                "Loyal Friend",
                "A supportive and loyal friend",
            ),
            (
                GrokPersonalityType::HomeworkHelper,
                "grok3_personality_homework_helper",
                "Homework Helper",
                "A patient tutor focused on helping with homework",
            ),
            (
                GrokPersonalityType::TrustedTherapist,
                "grok3_personality_trusted_therapist",
                "Trusted Therapist",
                "A compassionate therapeutic personality",
            ),
            (
                GrokPersonalityType::None,
                "",
                "Default (No Personality)",
                "Standard Grok personality",
            ),
        ];

        assert_eq!(GrokPersonalityType::ALL, cases.map(|case| case.0));
        for (personality_type, id, display_name, description) in cases {
            assert_eq!(personality_type.id(), id);
            assert_eq!(personality_type.display_name(), display_name);
            assert_eq!(personality_type.description(), description);
            assert_eq!(GrokPersonalityType::from_id(id), Some(personality_type));
        }
        assert_eq!(GrokPersonalityType::from_id("unknown"), None);
    }

    #[test]
    fn message_options_default_personality_is_none_like_swift() {
        assert_eq!(
            GrokMessageOptions::default().personality_type,
            GrokPersonalityType::None
        );
    }

    #[test]
    fn personality_type_serializes_as_swift_raw_value() -> Result<()> {
        let options = GrokMessageOptions {
            personality_type: GrokPersonalityType::TrustedTherapist,
            ..GrokMessageOptions::default()
        };

        let json = serde_json::to_value(&options)?;
        assert_eq!(
            json["personalityType"],
            "grok3_personality_trusted_therapist"
        );

        let decoded = serde_json::from_value::<GrokMessageOptions>(json)?;
        assert_eq!(
            decoded.personality_type,
            GrokPersonalityType::TrustedTherapist
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokTaskSchedule {
    pub task_cadence: String,
    pub is_enabled: bool,
    pub timezone: String,
    pub time_of_day: String,
    pub day_of_year: String,
}

impl GrokTaskSchedule {
    pub fn once(
        date: impl Into<String>,
        time: impl Into<String>,
        timezone: impl Into<String>,
    ) -> Self {
        Self {
            task_cadence: "TASK_CADENCE_ONCE".to_string(),
            is_enabled: true,
            timezone: timezone.into(),
            time_of_day: time.into(),
            day_of_year: date.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokTaskCreateOptions {
    pub name: String,
    pub metadata_json_string: String,
    pub schedule: Option<GrokTaskSchedule>,
    pub notification_method: String,
    pub model_mode: String,
    pub notification_decider_enable: bool,
    pub notification_decider_guideline: String,
    pub model_name: String,
    pub toolset: Vec<String>,
}

impl Default for GrokTaskCreateOptions {
    fn default() -> Self {
        Self {
            name: String::new(),
            metadata_json_string: "{}".to_string(),
            schedule: None,
            notification_method: "DEFAULT".to_string(),
            model_mode: "BASE".to_string(),
            notification_decider_enable: true,
            notification_decider_guideline: "only notify if it's economically valuable".to_string(),
            model_name: String::new(),
            toolset: vec![String::new()],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokWorkspaceCreateOptions {
    pub name: String,
    pub icon: String,
    pub custom_personality: String,
    pub preferred_model: String,
}

impl Default for GrokWorkspaceCreateOptions {
    fn default() -> Self {
        Self {
            name: "workspace".to_string(),
            icon: "l:book-open:lime".to_string(),
            custom_personality: "New PROJECT WORKSPACE".to_string(),
            preferred_model: "auto".to_string(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokWorkspaceListOptions {
    pub page_size: usize,
    pub order_by: String,
}

impl Default for GrokWorkspaceListOptions {
    fn default() -> Self {
        Self {
            page_size: 50,
            order_by: "ORDER_BY_LAST_USE_TIME".to_string(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokAssetListOptions {
    pub page_size: usize,
    pub order_by: String,
}

impl Default for GrokAssetListOptions {
    fn default() -> Self {
        Self {
            page_size: 9,
            order_by: "ORDER_BY_LAST_USE_TIME".to_string(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokConversationListOptions {
    pub page_size: usize,
    pub search_query: Option<String>,
}

impl Default for GrokConversationListOptions {
    fn default() -> Self {
        Self {
            page_size: 100,
            search_query: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokSpeechToTextOptions {
    pub audio_format: Option<String>,
    pub refinement_level: String,
}

impl Default for GrokSpeechToTextOptions {
    fn default() -> Self {
        Self {
            audio_format: None,
            refinement_level: DEFAULT_SPEECH_REFINEMENT_LEVEL.to_string(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokShareLinkOptions {
    pub page_size: usize,
    pub allow_indexing: bool,
}

impl Default for GrokShareLinkOptions {
    fn default() -> Self {
        Self {
            page_size: 100,
            allow_indexing: true,
        }
    }
}
