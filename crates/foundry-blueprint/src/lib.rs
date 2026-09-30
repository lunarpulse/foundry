//! foundry-blueprint — foundry.yaml parser + validation (plan Task 4).
//! Format references Warp's public v1alpha1 schema docs; no AGPL code.
use foundry_domain::order as _; // reserve: line→Order factory comes in T7
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum BlueprintError {
    #[error("yaml parse error: {0}")]
    Yaml(String),
    #[error("invalid blueprint: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct Blueprint {
    #[serde(rename = "schemaVersion")]
    pub schema_version: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub repositories: Vec<RepoRef>,
    #[serde(rename = "agentDefaults", default)]
    pub agent_defaults: AgentDefaults,
    #[serde(default)]
    pub lines: Vec<Line>,
    #[serde(default)]
    pub inspector: InspectorConfig,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RepoRef {
    pub owner: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AgentDefaults {
    #[serde(default = "default_profile")]
    pub profile: String,
}

fn default_profile() -> String {
    "coding".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct Line {
    pub name: String,
    #[serde(default)]
    pub intake: IntakeConfig,
    #[serde(default)]
    pub stages: Vec<String>,
    pub harness: HarnessSpec,
    #[serde(default)]
    pub approval: ApprovalPolicy,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct IntakeConfig {
    #[serde(default)]
    pub cron: Option<String>,
    #[serde(default)]
    pub label: Option<String>, // GitHub label intake
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct HarnessSpec {
    #[serde(rename = "type")]
    pub r#type: String,
    #[serde(default = "default_profile")]
    pub profile: String,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
pub enum ApprovalPolicy {
    #[default]
    #[serde(rename = "human")]
    Human,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct InspectorConfig {
    #[serde(default = "default_scorer")]
    pub scorer: String,
}

fn default_scorer() -> String {
    "default".into()
}

const SUPPORTED_SCHEMA: &str = "v1alpha1";

impl Blueprint {
    pub fn parse(yaml: &str) -> Result<Self, BlueprintError> {
        let bp: Blueprint =
            serde_yaml::from_str(yaml).map_err(|e| BlueprintError::Yaml(e.to_string()))?;
        bp.validate()?;
        Ok(bp)
    }

    fn validate(&self) -> Result<(), BlueprintError> {
        if self.schema_version != SUPPORTED_SCHEMA {
            return Err(BlueprintError::Invalid(format!(
                "unsupported schemaVersion: {} (support: {SUPPORTED_SCHEMA})",
                self.schema_version
            )));
        }
        if self.name.trim().is_empty() {
            return Err(BlueprintError::Invalid("name is required".into()));
        }
        // Note: a document missing `name:` fails here because serde's Option
        // is not used — `name` has no default, but serde_yaml gives "" for a
        // missing scalar only if the field is Option. Handle missing field:
        if self.name.is_empty() {
            return Err(BlueprintError::Invalid("name is required".into()));
        }
        if self.repositories.is_empty() {
            return Err(BlueprintError::Invalid(
                "at least one repository is required".into(),
            ));
        }
        for line in &self.lines {
            if line.harness.r#type.trim().is_empty() {
                return Err(BlueprintError::Invalid(format!(
                    "line '{}': harness.type is required",
                    line.name
                )));
            }
        }
        Ok(())
    }
}
