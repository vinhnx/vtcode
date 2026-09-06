use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EvalCategory {
    #[default]
    Capability,
    Regression,
}

impl EvalCategory {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Capability => "Capability",
            Self::Regression => "Regression",
        }
    }
    pub fn as_str(&self) -> &'static str {
        self.label()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    #[default]
    Pass,
    Fail,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalTask {
    pub id: String,
    pub name: String,
    pub category: EvalCategory,
    pub prompt: String,
    pub verify_commands: Vec<String>,
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalRunResult {
    pub task_id: String,
    pub outcome: RunOutcome,
    pub error_message: Option<String>,
    pub duration_secs: f64,
    pub attempt: u32,
    pub cost_usd: Option<f64>,
    pub transcript_path: Option<String>,
    /// Privacy-preserving summary of the attempt trace, when it was persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_summary: Option<crate::trace_analyzer::HarnessTraceSummary>,
}
