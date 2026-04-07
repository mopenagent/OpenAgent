//! SQLite persistence layer for the research service.
//!
//! Manages research sessions, tasks, dependency graph, and tool call logs.
//! Uses a bundled rusqlite (no external SQLite dep on the Pi).

use anyhow::{Context as _, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

// ── Default paths ──────────────────────────────────────────────────────────────

pub const DEFAULT_DB_PATH: &str = "data/research.db";
pub const DEFAULT_RESEARCH_DIR: &str = "data/research";
pub const DEFAULT_LOGS_DIR: &str = "logs";

// ── Helpers ────────────────────────────────────────────────────────────────────

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

// ── Domain types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Research {
    pub id: String,
    pub user_key: String,
    pub title: String,
    pub goal: String,
    pub status: String,
    pub is_current: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResearchTask {
    pub id: String,
    pub research_id: String,
    pub parent_id: Option<String>,
    pub description: String,
    pub status: String,
    pub assigned_agent: Option<String>,
    pub result: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCallRecord {
    pub id: String,
    pub tool_name: String,
    pub params: String,
    pub result: Option<String>,
    pub called_at: i64,
}

// ── Store ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ResearchStore {
    conn: Arc<Mutex<Connection>>,
}

impl ResearchStore {
    /// Open the SQLite database and run migrations.
    pub fn open(db_path: &Path, _research_dir: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create db dir {}", parent.display()))?;
        }
        let conn = Connection::open(db_path)
            .with_context(|| format!("open SQLite at {}", db_path.display()))?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.migrate()?;
        Ok(store)
    }

    /// Create all tables if they don't exist.
    pub fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            PRAGMA foreign_keys=ON;

            CREATE TABLE IF NOT EXISTS researches (
                id         TEXT PRIMARY KEY,
                user_key   TEXT NOT NULL,
                title      TEXT NOT NULL,
                goal       TEXT NOT NULL,
                status     TEXT NOT NULL DEFAULT 'active',
                is_current INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS research_tasks (
                id             TEXT PRIMARY KEY,
                research_id    TEXT NOT NULL REFERENCES researches(id),
                parent_id      TEXT REFERENCES research_tasks(id),
                description    TEXT NOT NULL,
                status         TEXT NOT NULL DEFAULT 'pending',
                assigned_agent TEXT,
                result         TEXT,
                created_at     INTEGER NOT NULL,
                updated_at     INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS research_task_deps (
                task_id    TEXT NOT NULL REFERENCES research_tasks(id),
                depends_on TEXT NOT NULL REFERENCES research_tasks(id),
                PRIMARY KEY (task_id, depends_on)
            );

            CREATE TABLE IF NOT EXISTS research_tool_calls (
                id          TEXT PRIMARY KEY,
                research_id TEXT NOT NULL REFERENCES researches(id),
                task_id     TEXT REFERENCES research_tasks(id),
                tool_name   TEXT NOT NULL,
                params      TEXT NOT NULL,
                result      TEXT,
                called_at   INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_researches_user_key  ON researches(user_key);
            CREATE INDEX IF NOT EXISTS idx_tasks_research_id    ON research_tasks(research_id);
            CREATE INDEX IF NOT EXISTS idx_tool_calls_research  ON research_tool_calls(research_id);
            ",
        )
        .context("migrate research schema")?;
        Ok(())
    }

    /// Create a new research for a user, making it the current one.
    pub fn create_research(&self, user_key: &str, title: &str, goal: &str) -> Result<Research> {
        let id = Uuid::new_v4().to_string();
        let now = now_ms();
        let mut conn = self.conn.lock().expect("db mutex poisoned");
        let tx = conn.transaction().context("begin create_research tx")?;
        tx.execute(
            "UPDATE researches SET is_current = 0 WHERE user_key = ?1",
            params![user_key],
        )
        .context("clear is_current")?;
        tx.execute(
            "INSERT INTO researches (id, user_key, title, goal, status, is_current, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'active', 1, ?5, ?5)",
            params![id, user_key, title, goal, now],
        )
        .context("insert research")?;
        tx.commit().context("commit create_research")?;
        Ok(Research {
            id,
            user_key: user_key.to_string(),
            title: title.to_string(),
            goal: goal.to_string(),
            status: "active".to_string(),
            is_current: true,
            created_at: now,
            updated_at: now,
        })
    }

    /// List all researches for a user, newest first.
    pub fn list_researches(&self, user_key: &str) -> Result<Vec<Research>> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, user_key, title, goal, status, is_current, created_at, updated_at
                 FROM researches WHERE user_key = ?1 ORDER BY updated_at DESC",
            )
            .context("prepare list_researches")?;
        let rows = stmt
            .query_map(params![user_key], |row| {
                Ok(Research {
                    id: row.get(0)?,
                    user_key: row.get(1)?,
                    title: row.get(2)?,
                    goal: row.get(3)?,
                    status: row.get(4)?,
                    is_current: row.get::<_, i64>(5)? != 0,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            })
            .context("query list_researches")?;
        rows.collect::<Result<Vec<_>, _>>().context("collect list_researches")
    }

    /// Get the current research for a user (if any).
    pub fn get_current(&self, user_key: &str) -> Result<Option<Research>> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, user_key, title, goal, status, is_current, created_at, updated_at
                 FROM researches WHERE user_key = ?1 AND is_current = 1 LIMIT 1",
            )
            .context("prepare get_current")?;
        let mut rows = stmt
            .query_map(params![user_key], |row| {
                Ok(Research {
                    id: row.get(0)?,
                    user_key: row.get(1)?,
                    title: row.get(2)?,
                    goal: row.get(3)?,
                    status: row.get(4)?,
                    is_current: row.get::<_, i64>(5)? != 0,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            })
            .context("query get_current")?;
        rows.next().transpose().context("get_current row")
    }

    /// Set the current research for a user.
    pub fn set_current(&self, user_key: &str, research_id: &str) -> Result<()> {
        let mut conn = self.conn.lock().expect("db mutex poisoned");
        let tx = conn.transaction().context("begin set_current tx")?;
        tx.execute(
            "UPDATE researches SET is_current = 0 WHERE user_key = ?1",
            params![user_key],
        )
        .context("clear is_current")?;
        let now = now_ms();
        tx.execute(
            "UPDATE researches SET is_current = 1, updated_at = ?1 WHERE id = ?2",
            params![now, research_id],
        )
        .context("set is_current")?;
        tx.commit().context("commit set_current")?;
        Ok(())
    }

    /// Update the status of a research.
    pub fn update_status(&self, research_id: &str, status: &str) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let now = now_ms();
        conn.execute(
            "UPDATE researches SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status, now, research_id],
        )
        .context("update research status")?;
        Ok(())
    }

    /// Add a task to a research session.
    pub fn add_task(
        &self,
        research_id: &str,
        description: &str,
        parent_id: Option<&str>,
        assigned_agent: Option<&str>,
        depends_on: &[String],
    ) -> Result<ResearchTask> {
        let id = Uuid::new_v4().to_string();
        let now = now_ms();
        let mut conn = self.conn.lock().expect("db mutex poisoned");
        let tx = conn.transaction().context("begin add_task tx")?;
        tx.execute(
            "INSERT INTO research_tasks (id, research_id, parent_id, description, status, assigned_agent, result, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5, NULL, ?6, ?6)",
            params![id, research_id, parent_id, description, assigned_agent, now],
        )
        .context("insert research_task")?;
        for dep in depends_on {
            tx.execute(
                "INSERT OR IGNORE INTO research_task_deps (task_id, depends_on) VALUES (?1, ?2)",
                params![id, dep],
            )
            .context("insert task dep")?;
        }
        tx.commit().context("commit add_task")?;
        Ok(ResearchTask {
            id,
            research_id: research_id.to_string(),
            parent_id: parent_id.map(str::to_string),
            description: description.to_string(),
            status: "pending".to_string(),
            assigned_agent: assigned_agent.map(str::to_string),
            result: None,
            created_at: now,
            updated_at: now,
        })
    }

    /// Update a task's status and optional result.
    pub fn update_task(&self, task_id: &str, status: &str, result: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let now = now_ms();
        conn.execute(
            "UPDATE research_tasks SET status = ?1, result = ?2, updated_at = ?3 WHERE id = ?4",
            params![status, result, now, task_id],
        )
        .context("update research_task")?;
        Ok(())
    }

    /// Get all tasks for a research, ordered by creation time.
    pub fn get_tasks(&self, research_id: &str) -> Result<Vec<ResearchTask>> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, research_id, parent_id, description, status, assigned_agent, result, created_at, updated_at
                 FROM research_tasks WHERE research_id = ?1 ORDER BY created_at ASC",
            )
            .context("prepare get_tasks")?;
        let rows = stmt
            .query_map(params![research_id], |row| {
                Ok(ResearchTask {
                    id: row.get(0)?,
                    research_id: row.get(1)?,
                    parent_id: row.get(2)?,
                    description: row.get(3)?,
                    status: row.get(4)?,
                    assigned_agent: row.get(5)?,
                    result: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            })
            .context("query get_tasks")?;
        rows.collect::<Result<Vec<_>, _>>().context("collect get_tasks")
    }

    /// Get the task dependencies for all tasks in a research.
    /// Returns Vec<(task_id, depends_on_task_id)>.
    pub fn get_task_deps(&self, research_id: &str) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT d.task_id, d.depends_on
                 FROM research_task_deps d
                 JOIN research_tasks t ON t.id = d.task_id
                 WHERE t.research_id = ?1",
            )
            .context("prepare get_task_deps")?;
        let rows = stmt
            .query_map(params![research_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .context("query get_task_deps")?;
        rows.collect::<Result<Vec<_>, _>>().context("collect get_task_deps")
    }

    /// Get the research for a specific task ID.
    pub fn get_research_for_task(&self, task_id: &str) -> Result<Option<Research>> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT r.id, r.user_key, r.title, r.goal, r.status, r.is_current, r.created_at, r.updated_at
                 FROM researches r
                 JOIN research_tasks t ON t.research_id = r.id
                 WHERE t.id = ?1 LIMIT 1",
            )
            .context("prepare get_research_for_task")?;
        let mut rows = stmt
            .query_map(params![task_id], |row| {
                Ok(Research {
                    id: row.get(0)?,
                    user_key: row.get(1)?,
                    title: row.get(2)?,
                    goal: row.get(3)?,
                    status: row.get(4)?,
                    is_current: row.get::<_, i64>(5)? != 0,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            })
            .context("query get_research_for_task")?;
        rows.next().transpose().context("get_research_for_task row")
    }

    /// Log a tool call associated with a research (called by tool handlers; allow dead_code for future use).
    #[allow(dead_code)]
    pub fn log_tool_call(
        &self,
        research_id: &str,
        task_id: Option<&str>,
        tool_name: &str,
        params_str: &str,
        result: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let id = Uuid::new_v4().to_string();
        let now = now_ms();
        conn.execute(
            "INSERT INTO research_tool_calls (id, research_id, task_id, tool_name, params, result, called_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, research_id, task_id, tool_name, params_str, result, now],
        )
        .context("insert tool call log")?;
        Ok(())
    }

    /// Get the most recent tool calls for a research.
    pub fn get_recent_tool_calls(
        &self,
        research_id: &str,
        limit: usize,
    ) -> Result<Vec<ToolCallRecord>> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, tool_name, params, result, called_at
                 FROM research_tool_calls
                 WHERE research_id = ?1
                 ORDER BY called_at DESC
                 LIMIT ?2",
            )
            .context("prepare get_recent_tool_calls")?;
        let rows = stmt
            .query_map(params![research_id, limit as i64], |row| {
                Ok(ToolCallRecord {
                    id: row.get(0)?,
                    tool_name: row.get(1)?,
                    params: row.get(2)?,
                    result: row.get(3)?,
                    called_at: row.get(4)?,
                })
            })
            .context("query get_recent_tool_calls")?;
        rows.collect::<Result<Vec<_>, _>>().context("collect get_recent_tool_calls")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Open an in-memory SQLite store — no temp files, no cleanup needed.
    fn mem_store() -> ResearchStore {
        ResearchStore::open(Path::new(":memory:"), Path::new("/tmp")).unwrap()
    }

    // ── create / get_current ────────────────────────────────────────────────

    #[test]
    fn create_research_is_returned_as_current() {
        let store = mem_store();
        let r = store.create_research("alice", "AI Safety", "Study alignment").unwrap();
        assert_eq!(r.user_key, "alice");
        assert_eq!(r.title, "AI Safety");
        assert!(r.is_current);
        assert_eq!(r.status, "active");

        let current = store.get_current("alice").unwrap().unwrap();
        assert_eq!(current.id, r.id);
    }

    #[test]
    fn no_research_get_current_returns_none() {
        let store = mem_store();
        assert!(store.get_current("nobody").unwrap().is_none());
    }

    #[test]
    fn second_create_clears_first_is_current() {
        let store = mem_store();
        let r1 = store.create_research("alice", "First", "Goal 1").unwrap();
        let r2 = store.create_research("alice", "Second", "Goal 2").unwrap();

        let current = store.get_current("alice").unwrap().unwrap();
        assert_eq!(current.id, r2.id, "second research should be current");

        // r1 must no longer be current
        let all = store.list_researches("alice").unwrap();
        let old = all.iter().find(|r| r.id == r1.id).unwrap();
        assert!(!old.is_current);
    }

    // ── list / set_current ──────────────────────────────────────────────────

    #[test]
    fn list_researches_newest_first() {
        let store = mem_store();
        store.create_research("bob", "Alpha", "Goal A").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        store.create_research("bob", "Beta", "Goal B").unwrap();

        let list = store.list_researches("bob").unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].title, "Beta");
        assert_eq!(list[1].title, "Alpha");
    }

    #[test]
    fn list_researches_is_user_scoped() {
        let store = mem_store();
        store.create_research("alice", "Alice R", "goal").unwrap();
        store.create_research("bob", "Bob R", "goal").unwrap();

        assert_eq!(store.list_researches("alice").unwrap().len(), 1);
        assert_eq!(store.list_researches("bob").unwrap().len(), 1);
        assert_eq!(store.list_researches("carol").unwrap().len(), 0);
    }

    #[test]
    fn set_current_switches_active_research() {
        let store = mem_store();
        let r1 = store.create_research("alice", "First", "G1").unwrap();
        let _r2 = store.create_research("alice", "Second", "G2").unwrap();

        // Switch back to r1
        store.set_current("alice", &r1.id).unwrap();
        let current = store.get_current("alice").unwrap().unwrap();
        assert_eq!(current.id, r1.id);
    }

    // ── update_status ───────────────────────────────────────────────────────

    #[test]
    fn update_status_changes_research_status() {
        let store = mem_store();
        let r = store.create_research("alice", "R", "G").unwrap();
        store.update_status(&r.id, "complete").unwrap();

        let list = store.list_researches("alice").unwrap();
        assert_eq!(list[0].status, "complete");
    }

    // ── tasks ───────────────────────────────────────────────────────────────

    #[test]
    fn add_task_appears_in_get_tasks() {
        let store = mem_store();
        let r = store.create_research("alice", "R", "G").unwrap();
        let t = store.add_task(&r.id, "Search papers", None, None, &[]).unwrap();

        let tasks = store.get_tasks(&r.id).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, t.id);
        assert_eq!(tasks[0].status, "pending");
        assert_eq!(tasks[0].description, "Search papers");
    }

    #[test]
    fn add_task_with_assigned_agent_stores_agent() {
        let store = mem_store();
        let r = store.create_research("alice", "R", "G").unwrap();
        let t = store.add_task(&r.id, "Retrieve data", None, Some("search-agent"), &[]).unwrap();

        let tasks = store.get_tasks(&r.id).unwrap();
        assert_eq!(tasks[0].id, t.id);
        assert_eq!(tasks[0].assigned_agent.as_deref(), Some("search-agent"));
    }

    #[test]
    fn update_task_changes_status_and_result() {
        let store = mem_store();
        let r = store.create_research("alice", "R", "G").unwrap();
        let t = store.add_task(&r.id, "Task", None, None, &[]).unwrap();

        store.update_task(&t.id, "done", Some("10 papers found")).unwrap();

        let tasks = store.get_tasks(&r.id).unwrap();
        assert_eq!(tasks[0].status, "done");
        assert_eq!(tasks[0].result.as_deref(), Some("10 papers found"));
    }

    // ── dependencies / runnable logic ───────────────────────────────────────

    #[test]
    fn task_deps_stored_and_retrieved() {
        let store = mem_store();
        let r = store.create_research("alice", "R", "G").unwrap();
        let t1 = store.add_task(&r.id, "First", None, None, &[]).unwrap();
        let t2 = store.add_task(&r.id, "Second", None, None, &[t1.id.clone()]).unwrap();

        let deps = store.get_task_deps(&r.id).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0], (t2.id.clone(), t1.id.clone()));
    }

    #[test]
    fn task_with_undone_dep_is_not_in_deps_done_set() {
        // Verify that when t1 is pending, its dep record is still stored —
        // the caller (handlers) is responsible for filtering runnable tasks.
        let store = mem_store();
        let r = store.create_research("alice", "R", "G").unwrap();
        let t1 = store.add_task(&r.id, "Blocking", None, None, &[]).unwrap();
        let t2 = store.add_task(&r.id, "Blocked", None, None, &[t1.id.clone()]).unwrap();

        let deps = store.get_task_deps(&r.id).unwrap();
        // dep record exists
        assert!(deps.iter().any(|(tid, dep)| tid == &t2.id && dep == &t1.id));
        // t1 is still pending
        let tasks = store.get_tasks(&r.id).unwrap();
        let t1_row = tasks.iter().find(|t| t.id == t1.id).unwrap();
        assert_eq!(t1_row.status, "pending");
    }

    #[test]
    fn done_dep_unblocks_dependent_in_runnable_simulation() {
        // Simulate the runnable-task logic from handle_research_status.
        let store = mem_store();
        let r = store.create_research("alice", "R", "G").unwrap();
        let t1 = store.add_task(&r.id, "Search", None, None, &[]).unwrap();
        let t2 = store.add_task(&r.id, "Analyse", None, None, &[t1.id.clone()]).unwrap();

        // Mark t1 done
        store.update_task(&t1.id, "done", Some("results")).unwrap();

        let tasks = store.get_tasks(&r.id).unwrap();
        let deps = store.get_task_deps(&r.id).unwrap();

        let done_ids: std::collections::HashSet<String> = tasks
            .iter()
            .filter(|t| t.status == "done")
            .map(|t| t.id.clone())
            .collect();
        let mut dep_map: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for (tid, dep) in &deps {
            dep_map.entry(tid.clone()).or_default().push(dep.clone());
        }

        let runnable: Vec<_> = tasks
            .iter()
            .filter(|t| {
                t.status == "pending"
                    && dep_map.get(&t.id).map_or(true, |ds| ds.iter().all(|d| done_ids.contains(d)))
            })
            .collect();

        assert_eq!(runnable.len(), 1);
        assert_eq!(runnable[0].id, t2.id);
    }

    // ── get_research_for_task ───────────────────────────────────────────────

    #[test]
    fn get_research_for_task_returns_parent_research() {
        let store = mem_store();
        let r = store.create_research("alice", "Parent Research", "G").unwrap();
        let t = store.add_task(&r.id, "Task", None, None, &[]).unwrap();

        let found = store.get_research_for_task(&t.id).unwrap().unwrap();
        assert_eq!(found.id, r.id);
        assert_eq!(found.title, "Parent Research");
    }

    #[test]
    fn get_research_for_nonexistent_task_returns_none() {
        let store = mem_store();
        assert!(store.get_research_for_task("no-such-id").unwrap().is_none());
    }
}
