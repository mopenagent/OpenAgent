/// Background scheduler — polls `cron_jobs` every `poll_secs` seconds,
/// executes due jobs, and records their results.
///
/// Shell jobs: spawned via `tokio::process::Command`.
/// Agent jobs: injected as synthetic `message.received` events into the
///             same broadcast channel the dispatch loop uses.  The agent
///             handles the prompt and the response is stored in the session;
///             result capture into `cron_runs` is best-effort (we mark the
///             job as run immediately after injection).
use crate::cron::{
    store::{due_jobs, record_run},
    types::{CronJob, JobType},
};
use serde_json::json;
use std::process::Stdio;
use tokio::process::Command;
use tokio::sync::broadcast;
use tokio::time::{self, Duration};
use tracing::{info, warn};

const MIN_POLL_SECS: u64 = 10;
const SHELL_TIMEOUT_SECS: u64 = 120;

/// Run the cron scheduler as a background task.
///
/// `event_tx` is the same broadcast sender used by `ServiceManager` — injecting
/// an event here causes the dispatch loop to pick it up and route it to the agent.
pub async fn run(
    db_path: String,
    poll_secs: u64,
    event_tx: broadcast::Sender<serde_json::Value>,
) {
    let poll_secs = poll_secs.max(MIN_POLL_SECS);
    let mut interval = time::interval(Duration::from_secs(poll_secs));
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

    info!(poll_secs, "cron.scheduler.start");

    loop {
        interval.tick().await;

        let now = chrono::Utc::now();
        let jobs = match due_jobs(&db_path, now) {
            Ok(j) => j,
            Err(e) => {
                warn!(error = %e, "cron.scheduler.query_failed");
                continue;
            }
        };

        if jobs.is_empty() {
            continue;
        }

        info!(count = jobs.len(), "cron.scheduler.jobs_due");

        for job in jobs {
            let db = db_path.clone();
            let tx = event_tx.clone();
            tokio::spawn(async move {
                execute_job(&db, &job, &tx).await;
            });
        }
    }
}

async fn execute_job(
    db_path: &str,
    job: &CronJob,
    event_tx: &broadcast::Sender<serde_json::Value>,
) {
    let started_at = chrono::Utc::now();
    info!(job_id = %job.id, job_type = ?job.job_type, "cron.job.start");

    let (success, output) = match job.job_type {
        JobType::Shell => run_shell_job(job).await,
        JobType::Agent => run_agent_job(job, event_tx),
    };

    let finished_at = chrono::Utc::now();
    let status = if success { "ok" } else { "error" };
    info!(
        job_id = %job.id,
        status,
        duration_ms = (finished_at - started_at).num_milliseconds(),
        "cron.job.done"
    );

    if let Err(e) = record_run(db_path, job, started_at, finished_at, success, &output) {
        warn!(job_id = %job.id, error = %e, "cron.job.record_run.failed");
    }
}

/// Execute a shell job with a hard timeout.  Returns (success, stdout+stderr).
async fn run_shell_job(job: &CronJob) -> (bool, String) {
    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(&job.command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(job_id = %job.id, error = %e, "cron.shell_job.spawn_failed");
            return (false, format!("spawn failed: {e}"));
        }
    };

    let timeout = Duration::from_secs(SHELL_TIMEOUT_SECS);

    // `wait_with_output` consumes `child`, so we kill via the OS pid on timeout.
    let pid = child.id();
    let result = tokio::time::timeout(timeout, child.wait_with_output()).await;

    match result {
        Ok(Ok(out)) => {
            let mut combined = String::new();
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            if !stdout.is_empty() {
                combined.push_str(&stdout);
            }
            if !stderr.is_empty() {
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str(&stderr);
            }
            (out.status.success(), combined)
        }
        Ok(Err(e)) => (false, format!("io error: {e}")),
        Err(_) => {
            // Best-effort kill — ignore errors (process may have already exited).
            if let Some(pid) = pid {
                let _ = tokio::process::Command::new("kill")
                    .arg(pid.to_string())
                    .status()
                    .await;
            }
            (false, format!("timed out after {SHELL_TIMEOUT_SECS}s"))
        }
    }
}

/// Inject a synthetic `message.received` event into the dispatch broadcast bus.
/// The dispatch loop picks this up, runs the agent, and sends the response to
/// the `cron://<job_id>` pseudo-channel (which is safely ignored by channels).
///
/// We mark the job as succeeded immediately — response capture is async and not
/// waited for.  A future phase can tap into the session to retrieve the output.
fn run_agent_job(
    job: &CronJob,
    event_tx: &broadcast::Sender<serde_json::Value>,
) -> (bool, String) {
    let prompt = match &job.prompt {
        Some(p) if !p.trim().is_empty() => p.clone(),
        _ => {
            warn!(job_id = %job.id, "cron.agent_job.no_prompt");
            return (false, "agent job has no prompt".to_string());
        }
    };

    // `cron://<job_id>` is the pseudo-channel URI.  The dispatch loop uses it
    // as the session_id, so each job gets an isolated conversation history.
    let channel = format!("cron://{}", job.id);

    let event = json!({
        "type": "event",
        "event": "message.received",
        "data": {
            "content": prompt,
            "channel": channel,
            "sender": "cron"
        }
    });

    match event_tx.send(event) {
        Ok(_) => (true, "agent job queued".to_string()),
        Err(e) => {
            warn!(job_id = %job.id, error = %e, "cron.agent_job.send_failed");
            (false, format!("failed to queue agent job: {e}"))
        }
    }
}
