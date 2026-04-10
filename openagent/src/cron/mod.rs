//! Cron module — persistent scheduled jobs for OpenAgent.
//!
//! Ported and adapted from zeroclaw `src/cron/`.  Jobs are stored in the
//! existing `data/openagent.db` SQLite database under `cron_jobs` and
//! `cron_runs` tables.
//!
//! # Schedule types
//!
//! | Kind    | Example                                    | Behaviour           |
//! |---------|---------------------------------------------|---------------------|
//! | `cron`  | `{"kind":"cron","expr":"0 9 * * 1-5"}`    | Recurring via expr  |
//! | `at`    | `{"kind":"at","at":"2026-12-31T09:00:00Z"}`| One-shot UTC time   |
//! | `every` | `{"kind":"every","every_ms":3600000}`       | Fixed interval      |
//!
//! # Job types
//!
//! | Type    | How it runs                                         |
//! |---------|-----------------------------------------------------|
//! | `shell` | `sh -c <command>` with 120 s timeout                |
//! | `agent` | Synthetic `message.received` event → dispatch loop  |
//!
//! # LLM access
//!
//! Cron tools are **not pinned** — the LLM discovers them via `agent.discover`.
//! They are built-in (in-process) tools: `ToolRouter` intercepts any `cron.*`
//! call and routes it to [`tools::handle`] without a TCP hop.
//!
//! Built-in entries are injected into the `ActionCatalog` at startup via
//! [`catalog_entries`], which calls [`tools::tool_schemas`].
//!
//! # Enabling
//!
//! Add to `config/openagent.toml`:
//! ```toml
//! [cron]
//! enabled   = true
//! poll_secs = 30      # how often the scheduler checks for due jobs
//! ```

pub mod schedule;
pub mod scheduler;
pub mod store;
pub mod tools;
pub mod types;

pub use schedule::{next_run_for_schedule, normalize_expression, validate_schedule};
pub use store::{
    add_agent_job, add_shell_job, due_jobs, get_job, init_tables, init_tables_at, list_jobs,
    list_runs, record_run, remove_job, update_job,
};
pub use types::{
    CronJob, CronJobPatch, CronRun, JobType, Schedule, deserialize_maybe_stringified,
};

use crate::agent::action::catalog::{ActionEntry, ActionKind};
use std::path::PathBuf;

/// Build `ActionEntry` objects for all cron.* tools so they appear in
/// `agent.discover`.  These entries have an empty `address` — `ToolRouter`
/// handles them in-process.
#[must_use]
pub fn catalog_entries() -> Vec<ActionEntry> {
    tools::tool_schemas()
        .into_iter()
        .map(|(name, description, params)| {
            let required: Vec<String> = params
                .get("required")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let param_names: Vec<String> = params
                .get("properties")
                .and_then(|v| v.as_object())
                .map(|obj| obj.keys().cloned().collect())
                .unwrap_or_default();

            ActionEntry {
                kind: ActionKind::Tool,
                owner: "cron".to_string(),
                runtime: "builtin".to_string(),
                manifest_path: PathBuf::new(),
                address: String::new(), // in-process — no TCP
                name: name.to_string(),
                summary: description.to_string(),
                params,
                required,
                param_names,
                allowed_tools: vec![],
                enforce: false,
                enabled: true,
                steps: vec![],
                constraints: vec![],
                completion_criteria: vec![],
                guidance: String::new(),
                search_blob: description.to_string(),
            }
        })
        .collect()
}
