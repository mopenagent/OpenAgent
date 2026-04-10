/// SQLite persistence for cron jobs and run history.
///
/// Uses the same `data/openagent.db` file as the session backend — tables are
/// prefixed `cron_` to avoid clashes.  A new connection is opened per call
/// (SQLite WAL mode allows concurrent readers + one writer without blocking).
use crate::cron::{
    schedule::{next_run_for_schedule, schedule_cron_expression, validate_schedule},
    types::{CronJob, CronJobPatch, CronRun, JobType, Schedule},
};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use uuid::Uuid;

const MAX_OUTPUT_BYTES: usize = 16 * 1024;

// ---------------------------------------------------------------------------
// Schema init
// ---------------------------------------------------------------------------

/// Ensure cron tables exist by opening a new connection to `db_path`.
/// Call this once at startup before the scheduler runs.
pub fn init_tables_at(db_path: &str) -> Result<()> {
    let conn = open(db_path)?;
    init_tables(&conn)
}

/// Ensure cron tables exist.  Called once at startup.
pub fn init_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS cron_jobs (
            id              TEXT PRIMARY KEY,
            expression      TEXT NOT NULL DEFAULT '',
            schedule        TEXT NOT NULL,
            job_type        TEXT NOT NULL DEFAULT 'shell',
            command         TEXT NOT NULL DEFAULT '',
            prompt          TEXT,
            name            TEXT,
            enabled         INTEGER NOT NULL DEFAULT 1,
            delete_after_run INTEGER NOT NULL DEFAULT 0,
            created_at      TEXT NOT NULL,
            next_run        TEXT NOT NULL,
            last_run        TEXT,
            last_status     TEXT,
            last_output     TEXT
        ) STRICT;

        CREATE TABLE IF NOT EXISTS cron_runs (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            job_id          TEXT NOT NULL REFERENCES cron_jobs(id) ON DELETE CASCADE,
            started_at      TEXT NOT NULL,
            finished_at     TEXT NOT NULL,
            status          TEXT NOT NULL,
            output          TEXT,
            duration_ms     INTEGER
        ) STRICT;

        CREATE INDEX IF NOT EXISTS cron_jobs_next_run ON cron_jobs(next_run)
            WHERE enabled = 1;
        CREATE INDEX IF NOT EXISTS cron_runs_job_id ON cron_runs(job_id);
        ",
    )
    .context("failed to create cron tables")
}

// ---------------------------------------------------------------------------
// Connection factory
// ---------------------------------------------------------------------------

fn open(db_path: &str) -> Result<Connection> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("failed to open cron db at {db_path}"))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    Ok(conn)
}

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

fn row_to_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<CronJob> {
    let schedule_json: String = row.get(2)?;
    let schedule: Schedule = serde_json::from_str(&schedule_json)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e)))?;

    let job_type_str: String = row.get(3)?;
    let job_type = JobType::try_from(job_type_str.as_str())
        .unwrap_or_default();

    let parse_dt = |s: Option<String>| -> Option<DateTime<Utc>> {
        s.and_then(|v| DateTime::parse_from_rfc3339(&v).ok().map(|d| d.with_timezone(&Utc)))
    };

    Ok(CronJob {
        id: row.get(0)?,
        expression: row.get(1)?,
        schedule,
        job_type,
        command: row.get(4)?,
        prompt: row.get(5)?,
        name: row.get(6)?,
        enabled: row.get::<_, i64>(7)? != 0,
        delete_after_run: row.get::<_, i64>(8)? != 0,
        created_at: row.get::<_, String>(9)?
            .parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
        next_run: row.get::<_, String>(10)?
            .parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
        last_run:    parse_dt(row.get(11)?),
        last_status: row.get(12)?,
        last_output: row.get(13)?,
    })
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

/// Return all enabled jobs whose `next_run` is at or before `now`.
pub fn due_jobs(db_path: &str, now: DateTime<Utc>) -> Result<Vec<CronJob>> {
    let conn = open(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT id, expression, schedule, job_type, command, prompt, name,
                enabled, delete_after_run, created_at, next_run,
                last_run, last_status, last_output
         FROM cron_jobs
         WHERE enabled = 1 AND next_run <= ?1
         ORDER BY next_run ASC",
    )?;
    let jobs = stmt
        .query_map(params![now.to_rfc3339()], row_to_job)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to query due cron jobs")?;
    Ok(jobs)
}

/// Return all jobs (for listing).
pub fn list_jobs(db_path: &str) -> Result<Vec<CronJob>> {
    let conn = open(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT id, expression, schedule, job_type, command, prompt, name,
                enabled, delete_after_run, created_at, next_run,
                last_run, last_status, last_output
         FROM cron_jobs ORDER BY next_run ASC",
    )?;
    let jobs = stmt
        .query_map([], row_to_job)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to list cron jobs")?;
    Ok(jobs)
}

/// Return a single job by id.
pub fn get_job(db_path: &str, id: &str) -> Result<CronJob> {
    let conn = open(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT id, expression, schedule, job_type, command, prompt, name,
                enabled, delete_after_run, created_at, next_run,
                last_run, last_status, last_output
         FROM cron_jobs WHERE id = ?1",
    )?;
    stmt.query_row(params![id], row_to_job)
        .with_context(|| format!("cron job not found: {id}"))
}

/// Return the last N run records for a job.
pub fn list_runs(db_path: &str, job_id: &str, limit: u32) -> Result<Vec<CronRun>> {
    let conn = open(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT id, job_id, started_at, finished_at, status, output, duration_ms
         FROM cron_runs WHERE job_id = ?1 ORDER BY id DESC LIMIT ?2",
    )?;
    let runs = stmt
        .query_map(params![job_id, limit], |row| {
            Ok(CronRun {
                id: row.get(0)?,
                job_id: row.get(1)?,
                started_at: row
                    .get::<_, String>(2)?
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now()),
                finished_at: row
                    .get::<_, String>(3)?
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now()),
                status: row.get(4)?,
                output: row.get(5)?,
                duration_ms: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to list cron runs")?;
    Ok(runs)
}

// ---------------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------------

/// Insert a shell job and return it.
pub fn add_shell_job(
    db_path: &str,
    name: Option<String>,
    schedule: Schedule,
    command: &str,
) -> Result<CronJob> {
    let now = Utc::now();
    validate_schedule(&schedule, now)?;
    let next_run = next_run_for_schedule(&schedule, now)?;
    let id = Uuid::new_v4().to_string();
    let expression = schedule_cron_expression(&schedule).unwrap_or_default();
    let schedule_json = serde_json::to_string(&schedule)?;
    let delete_after_run = matches!(schedule, Schedule::At { .. });

    let conn = open(db_path)?;
    conn.execute(
        "INSERT INTO cron_jobs
            (id, expression, schedule, job_type, command, prompt, name,
             enabled, delete_after_run, created_at, next_run)
         VALUES (?1, ?2, ?3, 'shell', ?4, NULL, ?5, 1, ?6, ?7, ?8)",
        params![
            id,
            expression,
            schedule_json,
            command,
            name,
            i64::from(delete_after_run),
            now.to_rfc3339(),
            next_run.to_rfc3339(),
        ],
    )
    .context("failed to insert shell cron job")?;

    get_job(db_path, &id)
}

/// Insert an agent job (prompt sent to the in-process agent on schedule).
pub fn add_agent_job(
    db_path: &str,
    name: Option<String>,
    schedule: Schedule,
    prompt: &str,
    delete_after_run: bool,
) -> Result<CronJob> {
    let now = Utc::now();
    validate_schedule(&schedule, now)?;
    let next_run = next_run_for_schedule(&schedule, now)?;
    let id = Uuid::new_v4().to_string();
    let expression = schedule_cron_expression(&schedule).unwrap_or_default();
    let schedule_json = serde_json::to_string(&schedule)?;

    let conn = open(db_path)?;
    conn.execute(
        "INSERT INTO cron_jobs
            (id, expression, schedule, job_type, command, prompt, name,
             enabled, delete_after_run, created_at, next_run)
         VALUES (?1, ?2, ?3, 'agent', '', ?4, ?5, 1, ?6, ?7, ?8)",
        params![
            id,
            expression,
            schedule_json,
            prompt,
            name,
            i64::from(delete_after_run),
            now.to_rfc3339(),
            next_run.to_rfc3339(),
        ],
    )
    .context("failed to insert agent cron job")?;

    get_job(db_path, &id)
}

/// Remove a job by id.  Returns an error if the job does not exist.
pub fn remove_job(db_path: &str, id: &str) -> Result<()> {
    let conn = open(db_path)?;
    let rows = conn
        .execute("DELETE FROM cron_jobs WHERE id = ?1", params![id])
        .context("failed to delete cron job")?;
    anyhow::ensure!(rows > 0, "cron job not found: {id}");
    Ok(())
}

/// Apply a partial patch to a job.  Only `Some` fields are updated.
pub fn update_job(db_path: &str, id: &str, patch: CronJobPatch) -> Result<CronJob> {
    let conn = open(db_path)?;
    let existing = get_job(db_path, id)?;

    let schedule = patch.schedule.as_ref().unwrap_or(&existing.schedule);
    let now = Utc::now();
    let next_run = next_run_for_schedule(schedule, now)?;
    let expression = schedule_cron_expression(schedule).unwrap_or(existing.expression.clone());
    let schedule_json = serde_json::to_string(schedule)?;

    let command  = patch.command.as_deref().unwrap_or(&existing.command);
    let prompt   = patch.prompt.as_deref().or(existing.prompt.as_deref());
    let name     = patch.name.as_deref().or(existing.name.as_deref());
    let enabled  = patch.enabled.unwrap_or(existing.enabled);
    let delete   = patch.delete_after_run.unwrap_or(existing.delete_after_run);

    conn.execute(
        "UPDATE cron_jobs
         SET expression = ?1, schedule = ?2, command = ?3, prompt = ?4,
             name = ?5, enabled = ?6, delete_after_run = ?7, next_run = ?8
         WHERE id = ?9",
        params![
            expression,
            schedule_json,
            command,
            prompt,
            name,
            i64::from(enabled),
            i64::from(delete),
            next_run.to_rfc3339(),
            id,
        ],
    )
    .context("failed to update cron job")?;

    get_job(db_path, id)
}

/// Record that a job has finished (updates last_run, last_status, last_output)
/// and advances next_run (or deletes if delete_after_run).
pub fn record_run(
    db_path: &str,
    job: &CronJob,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    success: bool,
    output: &str,
) -> Result<()> {
    let status = if success { "ok" } else { "error" };
    let duration_ms = (finished_at - started_at).num_milliseconds();
    let truncated_output = if output.len() > MAX_OUTPUT_BYTES {
        format!("{}\n...[truncated]", &output[..MAX_OUTPUT_BYTES])
    } else {
        output.to_string()
    };

    let conn = open(db_path)?;

    conn.execute(
        "INSERT INTO cron_runs (job_id, started_at, finished_at, status, output, duration_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            job.id,
            started_at.to_rfc3339(),
            finished_at.to_rfc3339(),
            status,
            truncated_output,
            duration_ms,
        ],
    )
    .context("failed to insert cron run")?;

    if job.delete_after_run && success {
        conn.execute("DELETE FROM cron_jobs WHERE id = ?1", params![job.id])
            .context("failed to delete one-shot cron job")?;
        return Ok(());
    }

    let next_run = next_run_for_schedule(&job.schedule, Utc::now())?;
    conn.execute(
        "UPDATE cron_jobs
         SET last_run = ?1, last_status = ?2, last_output = ?3, next_run = ?4
         WHERE id = ?5",
        params![
            finished_at.to_rfc3339(),
            status,
            truncated_output,
            next_run.to_rfc3339(),
            job.id,
        ],
    )
    .context("failed to reschedule cron job")?;

    Ok(())
}
