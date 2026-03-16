use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db;

/// `guard.check` — returns `{"allowed": bool, "reason": "..."}`.
///
/// `reason` is one of:
/// - `"platform_bypass"` — web/whatsapp platform, always allowed (no whitelist check)
/// - `"whitelisted"` — sender found in the whitelist
/// - `"blocked"` — sender not in the whitelist; also recorded in `seen_senders`
pub fn handle_check(conn: &Connection, params: Value) -> Result<String> {
    let platform = params
        .get("platform")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing required param: platform"))?;
    let channel_id = params
        .get("channel_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing required param: channel_id"))?;

    // Web: local UI only, no external exposure.
    // WhatsApp: access controlled by WhatsApp itself; the @lid whitelist format from
    // the previous Python/neonize era doesn't match whatsmeow @s.whatsapp.net JIDs.
    // Bypassed until the whitelist is rebuilt with the correct JID format.
    if platform == "web" || platform == "whatsapp" {
        return Ok(json!({"allowed": true, "reason": "platform_bypass"}).to_string());
    }

    let allowed = db::check(conn, platform, channel_id)?;

    if allowed {
        Ok(json!({"allowed": true, "reason": "whitelisted"}).to_string())
    } else {
        // Record so admins can see who was blocked and easily add them.
        let _ = db::record_seen(conn, platform, channel_id);
        Ok(json!({"allowed": false, "reason": "blocked"}).to_string())
    }
}

/// `guard.add` — adds or updates a whitelist entry.
pub fn handle_add(conn: &Connection, params: Value) -> Result<String> {
    let platform = params
        .get("platform")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing required param: platform"))?;
    let channel_id = params
        .get("channel_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing required param: channel_id"))?;
    // Accept either "label" (openagent.db schema) or "note" (legacy) as the annotation.
    let label = params
        .get("label")
        .or_else(|| params.get("note"))
        .and_then(Value::as_str);

    db::add(conn, platform, channel_id, label)?;
    Ok(json!({"ok": true, "platform": platform, "channel_id": channel_id}).to_string())
}

/// `guard.remove` — removes a whitelist entry.
pub fn handle_remove(conn: &Connection, params: Value) -> Result<String> {
    let platform = params
        .get("platform")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing required param: platform"))?;
    let channel_id = params
        .get("channel_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing required param: channel_id"))?;

    let removed = db::remove(conn, platform, channel_id)?;
    Ok(json!({"ok": removed, "platform": platform, "channel_id": channel_id}).to_string())
}

/// `guard.list` — returns all whitelist entries newest-first.
pub fn handle_list(conn: &Connection) -> Result<String> {
    let entries = db::list(conn)?;
    let count = entries.len();
    Ok(json!({"entries": entries, "count": count}).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE whitelist (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 platform TEXT NOT NULL,
                 channel_id TEXT NOT NULL,
                 label TEXT NOT NULL DEFAULT '',
                 added_by TEXT NOT NULL DEFAULT '',
                 added_at TEXT NOT NULL,
                 UNIQUE(platform, channel_id)
             );
             CREATE TABLE seen_senders (
                 platform TEXT NOT NULL,
                 channel_id TEXT NOT NULL,
                 first_seen INTEGER NOT NULL,
                 last_seen INTEGER NOT NULL,
                 hit_count INTEGER NOT NULL DEFAULT 1,
                 PRIMARY KEY (platform, channel_id)
             );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn web_always_allowed() {
        let conn = mem_db();
        let result: Value = serde_json::from_str(
            &handle_check(&conn, json!({"platform": "web", "channel_id": "browser"})).unwrap(),
        )
        .unwrap();
        assert_eq!(result["allowed"], true);
        assert_eq!(result["reason"], "platform_bypass");
    }

    #[test]
    fn blocked_sender_recorded_in_seen() {
        let conn = mem_db();
        let result: Value = serde_json::from_str(
            &handle_check(&conn, json!({"platform": "telegram", "channel_id": "stranger"}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["allowed"], false);
        assert_eq!(result["reason"], "blocked");

        let count: i64 = conn
            .query_row(
                "SELECT hit_count FROM seen_senders WHERE platform='telegram' AND channel_id='stranger'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn add_then_check_allowed() {
        let conn = mem_db();
        handle_add(&conn, json!({"platform": "discord", "channel_id": "alice", "label": "test"}))
            .unwrap();
        let result: Value = serde_json::from_str(
            &handle_check(&conn, json!({"platform": "discord", "channel_id": "alice"})).unwrap(),
        )
        .unwrap();
        assert_eq!(result["allowed"], true);
        assert_eq!(result["reason"], "whitelisted");
    }

    #[test]
    fn remove_returns_ok_false_when_missing() {
        let conn = mem_db();
        let result: Value = serde_json::from_str(
            &handle_remove(&conn, json!({"platform": "slack", "channel_id": "nobody"})).unwrap(),
        )
        .unwrap();
        assert_eq!(result["ok"], false);
    }

    #[test]
    fn list_count_matches_entries() {
        let conn = mem_db();
        handle_add(&conn, json!({"platform": "telegram", "channel_id": "a"})).unwrap();
        handle_add(&conn, json!({"platform": "telegram", "channel_id": "b"})).unwrap();
        let result: Value =
            serde_json::from_str(&handle_list(&conn).unwrap()).unwrap();
        assert_eq!(result["count"], 2);
        assert_eq!(result["entries"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn missing_platform_param_errors() {
        let conn = mem_db();
        assert!(handle_check(&conn, json!({"channel_id": "x"})).is_err());
    }
}
