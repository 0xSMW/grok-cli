use clap::Args;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputFormat {
    Markdown,
    Raw,
    Json,
}

impl OutputFormat {
    pub fn status_name(self) -> &'static str {
        match self {
            Self::Markdown => "MD",
            Self::Raw => "Raw",
            Self::Json => "JSON",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Markdown => "Markdown",
            Self::Raw => "Raw",
            Self::Json => "JSON",
        }
    }

    pub fn resolve(raw_value: &str) -> Option<Self> {
        match raw_value.trim().to_lowercase().as_str() {
            "md" | "markdown" => Some(Self::Markdown),
            "raw" | "plain" | "text" => Some(Self::Raw),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    pub fn is_json(self) -> bool {
        self == Self::Json
    }
}

#[derive(Clone, Debug, Args, Default)]
pub struct GrokCommandOptions {
    #[arg(long, hide = true)]
    pub reasoning: bool,

    #[arg(long, hide = true)]
    pub deep_search: bool,

    #[arg(long, hide = true)]
    pub no_search: bool,

    #[arg(short, long)]
    pub markdown: bool,

    #[arg(long)]
    pub raw: bool,

    #[arg(long)]
    pub json: bool,

    #[arg(long)]
    pub format: Option<String>,

    #[arg(long)]
    pub debug: bool,

    #[arg(long, hide = true)]
    pub no_custom_instructions: bool,

    #[arg(long = "private")]
    pub private_mode: bool,

    #[arg(long)]
    pub stream: bool,

    #[arg(long, alias = "mode")]
    pub model: Option<String>,
}

impl GrokCommandOptions {
    pub fn merged(&self, command_options: &Self) -> Self {
        Self {
            reasoning: self.reasoning || command_options.reasoning,
            deep_search: self.deep_search || command_options.deep_search,
            no_search: self.no_search || command_options.no_search,
            markdown: self.markdown || command_options.markdown,
            raw: self.raw || command_options.raw,
            json: self.json || command_options.json,
            format: command_options
                .format
                .clone()
                .or_else(|| self.format.clone()),
            debug: self.debug || command_options.debug,
            no_custom_instructions: self.no_custom_instructions
                || command_options.no_custom_instructions,
            private_mode: self.private_mode || command_options.private_mode,
            stream: self.stream || command_options.stream,
            model: command_options.model.clone().or_else(|| self.model.clone()),
        }
    }

    pub fn resolved_output_format(&self) -> anyhow::Result<OutputFormat> {
        if let Some(format) = self.format.as_deref() {
            return OutputFormat::resolve(format).ok_or_else(|| {
                anyhow::anyhow!("Invalid --format value: {format}. Use md, raw, or json.")
            });
        }

        if self.json {
            return Ok(OutputFormat::Json);
        }

        if self.raw {
            return Ok(OutputFormat::Raw);
        }

        Ok(OutputFormat::Markdown)
    }

    pub fn warnings(&self) -> Vec<String> {
        Self::warnings_for(
            self.reasoning,
            self.deep_search,
            self.no_search,
            self.no_custom_instructions,
        )
    }

    pub fn warnings_for(
        reasoning: bool,
        deep_search: bool,
        no_search: bool,
        no_custom_instructions: bool,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        if reasoning {
            warnings.push("--reasoning is deprecated and ignored since the Grok 4 release on 2025-07-09; reasoning is always enabled for all models.".to_string());
        }
        if deep_search {
            warnings.push(
                "--deep-search is deprecated and ignored; deep research is no longer a Grok 4 feature."
                    .to_string(),
            );
        }
        if no_search {
            warnings.push(
                "--no-search is deprecated and ignored; Grok 4 search is automatic and no longer configurable."
                    .to_string(),
            );
        }
        if no_custom_instructions {
            warnings.push(
                "--no-custom-instructions is deprecated and ignored; instructions are managed in Grok agent settings."
                    .to_string(),
            );
        }
        warnings
    }
}

#[cfg(test)]
mod tests {
    use super::OutputFormat;

    #[test]
    fn resolves_swift_output_format_aliases() {
        assert_eq!(OutputFormat::resolve("md"), Some(OutputFormat::Markdown));
        assert_eq!(
            OutputFormat::resolve("markdown"),
            Some(OutputFormat::Markdown)
        );
        assert_eq!(OutputFormat::resolve("raw"), Some(OutputFormat::Raw));
        assert_eq!(OutputFormat::resolve("plain"), Some(OutputFormat::Raw));
        assert_eq!(OutputFormat::resolve("text"), Some(OutputFormat::Raw));
        assert_eq!(OutputFormat::resolve("json"), Some(OutputFormat::Json));
        assert_eq!(OutputFormat::resolve("bogus"), None);
    }
}
