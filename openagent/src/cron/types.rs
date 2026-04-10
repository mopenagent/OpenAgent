use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Try to deserialise a `serde_json::Value` as `T`.
/// If the value is a JSON string that contains an object (LLM double-serialised
/// it), parse the inner string first.  Provides backward-compatible handling for
/// both `Value::Object` and `Value::String` representations.
pub fn deserialize_maybe_stringified<T: serde::de::DeserializeOwned>(
    v: &serde_json::Value,
) -> Result<T, serde_json::Error> {
    match serde_json::from_value::<T>(v.clone()) {
        Ok(parsed) => Ok(parsed),
        Err(first_err) => {
            if let Some(s) = v.as_str() {
                let s = s.trim();
                if s.starts_with('{') || s.starts_with('[') {
                    if let Ok(inner) = serde_json::from_str::<serde_json::Value>(s) {
                        return serde_json::from_value::<T>(inner);
                    }
                }
            }
            Err(first_err)
        }
    }
}

// ---------------------------------------------------------------------------
// JobType
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum JobType {
    #[default]
    Shell,
    Agent,
}

impl From<JobType> for &'static str {
    fn from(value: JobType) -> Self {
        match value {
            JobType::Shell => "shell",
            JobType::Agent => "agent",
        }
    }
}

impl TryFrom<&str> for JobType {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value.to_lowercase().as_str() {
            "shell" => Ok(Self::Shell),
            "agent" => Ok(Self::Agent),
            _ => Err(format!(
                "Invalid job type '{value}'. Expected one of: 'shell', 'agent'"
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Schedule
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Schedule {
    /// Standard 5-field cron expression with optional IANA timezone.
    Cron {
        expr: String,
        #[serde(default)]
        tz: Option<String>,
    },
    /// One-shot: fire once at the given UTC instant then optionally self-delete.
    At { at: DateTime<Utc> },
    /// Fixed interval in milliseconds (e.g. 3_600_000 = every hour).
    Every { every_ms: u64 },
}

// ---------------------------------------------------------------------------
// CronJob
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: String,
    /// Human-friendly cron expression string (empty for At/Every schedules).
    pub expression: String,
    pub schedule: Schedule,
    /// Shell command (job_type = Shell) or empty string (job_type = Agent).
    pub command: String,
    /// Agent prompt (job_type = Agent) or None (job_type = Shell).
    pub prompt: Option<String>,
    pub name: Option<String>,
    pub job_type: JobType,
    pub enabled: bool,
    /// Auto-delete after first successful run (default true for At schedules).
    pub delete_after_run: bool,
    pub created_at: DateTime<Utc>,
    pub next_run: DateTime<Utc>,
    pub last_run: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub last_output: Option<String>,
}

// ---------------------------------------------------------------------------
// CronJobPatch (for updates)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CronJobPatch {
    pub schedule: Option<Schedule>,
    pub command: Option<String>,
    pub prompt: Option<String>,
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub delete_after_run: Option<bool>,
}

// ---------------------------------------------------------------------------
// CronRun (execution history)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronRun {
    pub id: i64,
    pub job_id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub status: String,
    pub output: Option<String>,
    pub duration_ms: Option<i64>,
}
