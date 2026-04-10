/// In-process tool handlers for the cron module.
///
/// These are exposed to the LLM via `agent.discover` (not pinned — the LLM
/// discovers them on demand using the tool catalog).  `ToolRouter` intercepts
/// any `cron.*` call and routes it here instead of making a TCP hop.
///
/// Tool surface:
///   cron.add    — create a shell or agent cron job
///   cron.list   — list all scheduled jobs
///   cron.get    — fetch one job by id
///   cron.remove — delete a job
///   cron.update — patch schedule / command / name / enabled
///   cron.run    — trigger a job immediately (outside its schedule)
///   cron.runs   — show execution history for a job
use crate::cron::{
    store::{
        add_agent_job, add_shell_job, get_job, list_jobs, list_runs, record_run, remove_job,
        update_job,
    },
    types::{CronJobPatch, JobType, Schedule, deserialize_maybe_stringified},
};
use anyhow::Result;
use serde_json::{Value, json};
use tracing::info;

/// Dispatch an in-process cron tool call.  Returns the result string (JSON or
/// plain text) to be forwarded back to the LLM as a `tool.result` frame.
pub async fn handle(db_path: &str, tool: &str, args: &Value) -> Result<String> {
    match tool {
        "cron.add"    => handle_add(db_path, args),
        "cron.list"   => handle_list(db_path),
        "cron.get"    => handle_get(db_path, args),
        "cron.remove" => handle_remove(db_path, args),
        "cron.update" => handle_update(db_path, args),
        "cron.run"    => handle_run_now(db_path, args).await,
        "cron.runs"   => handle_runs(db_path, args),
        other => anyhow::bail!("unknown cron tool: {other}"),
    }
}

fn handle_add(db_path: &str, args: &Value) -> Result<String> {
    let schedule = match args.get("schedule") {
        Some(v) => deserialize_maybe_stringified::<Schedule>(v)
            .map_err(|e| anyhow::anyhow!("invalid schedule: {e}"))?,
        None => anyhow::bail!("missing 'schedule' parameter"),
    };

    let name = args.get("name").and_then(Value::as_str).map(str::to_string);

    let job_type = match args.get("job_type").and_then(Value::as_str) {
        Some("agent") => JobType::Agent,
        Some("shell") | None => JobType::Shell,
        Some(other) => anyhow::bail!("invalid job_type: {other}"),
    };

    let job = match job_type {
        JobType::Shell => {
            let command = args
                .get("command")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("missing 'command' for shell job"))?;
            add_shell_job(db_path, name, schedule, command)?
        }
        JobType::Agent => {
            let prompt = args
                .get("prompt")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("missing 'prompt' for agent job"))?;
            let delete_after_run = matches!(
                args.get("delete_after_run").and_then(Value::as_bool),
                Some(true)
            ) || matches!(schedule, Schedule::At { .. });
            add_agent_job(db_path, name, schedule, prompt, delete_after_run)?
        }
    };

    info!(job_id = %job.id, job_type = ?job.job_type, next_run = %job.next_run, "cron.add");
    Ok(serde_json::to_string_pretty(&json!({
        "id":       job.id,
        "name":     job.name,
        "job_type": job.job_type,
        "schedule": job.schedule,
        "next_run": job.next_run,
        "enabled":  job.enabled,
    }))?)
}

fn handle_list(db_path: &str) -> Result<String> {
    let jobs = list_jobs(db_path)?;
    Ok(serde_json::to_string_pretty(&jobs)?)
}

fn handle_get(db_path: &str, args: &Value) -> Result<String> {
    let id = args
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing 'id'"))?;
    let job = get_job(db_path, id)?;
    Ok(serde_json::to_string_pretty(&job)?)
}

fn handle_remove(db_path: &str, args: &Value) -> Result<String> {
    let id = args
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing 'id'"))?;
    remove_job(db_path, id)?;
    info!(job_id = %id, "cron.remove");
    Ok(format!("removed cron job {id}"))
}

fn handle_update(db_path: &str, args: &Value) -> Result<String> {
    let id = args
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing 'id'"))?;

    let schedule = args
        .get("schedule")
        .map(|v| deserialize_maybe_stringified::<Schedule>(v))
        .transpose()
        .map_err(|e| anyhow::anyhow!("invalid schedule: {e}"))?;

    let patch = CronJobPatch {
        schedule,
        command:         args.get("command").and_then(Value::as_str).map(str::to_string),
        prompt:          args.get("prompt").and_then(Value::as_str).map(str::to_string),
        name:            args.get("name").and_then(Value::as_str).map(str::to_string),
        enabled:         args.get("enabled").and_then(Value::as_bool),
        delete_after_run: args.get("delete_after_run").and_then(Value::as_bool),
    };

    let job = update_job(db_path, id, patch)?;
    info!(job_id = %job.id, "cron.update");
    Ok(serde_json::to_string_pretty(&json!({
        "id":       job.id,
        "schedule": job.schedule,
        "next_run": job.next_run,
        "enabled":  job.enabled,
    }))?)
}

/// Trigger a job immediately, bypassing its schedule.
async fn handle_run_now(db_path: &str, args: &Value) -> Result<String> {
    let id = args
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing 'id'"))?;
    let job = get_job(db_path, id)?;

    if matches!(job.job_type, JobType::Agent) {
        return Ok(format!(
            "agent job '{id}' will be queued at next scheduler tick — use the scheduler to trigger it"
        ));
    }

    // Shell: run synchronously and record result.
    let started_at = chrono::Utc::now();
    use std::process::Stdio;
    let out = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&job.command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("failed to run job: {e}"))?;

    let finished_at = chrono::Utc::now();
    let mut output = String::new();
    output.push_str(&String::from_utf8_lossy(&out.stdout));
    if !out.stderr.is_empty() {
        output.push('\n');
        output.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    let success = out.status.success();
    record_run(db_path, &job, started_at, finished_at, success, &output)?;
    info!(job_id = %id, success, "cron.run_now");

    Ok(serde_json::to_string_pretty(&json!({
        "job_id":  id,
        "success": success,
        "output":  output,
        "duration_ms": (finished_at - started_at).num_milliseconds(),
    }))?)
}

fn handle_runs(db_path: &str, args: &Value) -> Result<String> {
    let job_id = args
        .get("job_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing 'job_id'"))?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|n| n as u32)
        .unwrap_or(20);
    let runs = list_runs(db_path, job_id, limit)?;
    Ok(serde_json::to_string_pretty(&runs)?)
}

// ---------------------------------------------------------------------------
// Tool schemas (used by ActionCatalog to build built-in entries)
// ---------------------------------------------------------------------------

/// Return the JSON tool schemas for all cron.* tools.
/// These are injected into the ActionCatalog as built-in entries so the LLM
/// can discover them via `agent.discover`.
#[must_use]
pub fn tool_schemas() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "cron.add",
            "Schedule a shell command or agent prompt to run on a cron/interval/at schedule. \
             Use job_type='agent' with a prompt to run the AI agent on schedule.",
            json!({
                "type": "object",
                "properties": {
                    "schedule": {
                        "description": "When to run. One of: {\"kind\":\"cron\",\"expr\":\"0 9 * * 1-5\"} | {\"kind\":\"at\",\"at\":\"2026-12-31T09:00:00Z\"} | {\"kind\":\"every\",\"every_ms\":3600000}",
                        "oneOf": [
                            { "type": "object", "properties": { "kind": { "type": "string", "enum": ["cron"] }, "expr": { "type": "string" }, "tz": { "type": "string" } }, "required": ["kind", "expr"] },
                            { "type": "object", "properties": { "kind": { "type": "string", "enum": ["at"] }, "at": { "type": "string" } }, "required": ["kind", "at"] },
                            { "type": "object", "properties": { "kind": { "type": "string", "enum": ["every"] }, "every_ms": { "type": "integer" } }, "required": ["kind", "every_ms"] }
                        ]
                    },
                    "job_type": { "type": "string", "enum": ["shell", "agent"], "description": "shell=run a command; agent=run AI agent with a prompt" },
                    "command": { "type": "string", "description": "Shell command (required when job_type=shell)" },
                    "prompt": { "type": "string", "description": "Agent prompt (required when job_type=agent)" },
                    "name": { "type": "string", "description": "Optional human-readable name" },
                    "delete_after_run": { "type": "boolean", "description": "Auto-delete after first successful run (default true for 'at' schedules)" }
                },
                "required": ["schedule"]
            }),
        ),
        (
            "cron.list",
            "List all scheduled cron jobs with their next_run times and status.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        ),
        (
            "cron.get",
            "Fetch a single cron job by id.",
            json!({ "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] }),
        ),
        (
            "cron.remove",
            "Delete a scheduled cron job by id.",
            json!({ "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] }),
        ),
        (
            "cron.update",
            "Update a cron job's schedule, command, prompt, name, or enabled flag.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "schedule": { "description": "New schedule (same format as cron.add)" },
                    "command": { "type": "string" },
                    "prompt": { "type": "string" },
                    "name": { "type": "string" },
                    "enabled": { "type": "boolean" }
                },
                "required": ["id"]
            }),
        ),
        (
            "cron.run",
            "Trigger a shell cron job immediately (outside its schedule). Agent jobs are queued.",
            json!({ "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] }),
        ),
        (
            "cron.runs",
            "List execution history for a cron job. Returns up to 'limit' most recent runs.",
            json!({
                "type": "object",
                "properties": {
                    "job_id": { "type": "string" },
                    "limit": { "type": "integer", "description": "Max results (default 20)" }
                },
                "required": ["job_id"]
            }),
        ),
    ]
}
