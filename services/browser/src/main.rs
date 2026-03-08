//! Browser service — MCP-lite wrapper for agent-browser CLI.
//!
//! Uses agent-browser's built-in `--session <id>` flag for persistent, isolated
//! browser sessions.  Each session keeps its own cookies, storage, and history.
//! Screenshots are written to `data/artifacts/browser/<session_id>/latest.png`.
//!
//! Install agent-browser first:
//!   npm install -g agent-browser
//!   agent-browser install        # download Chromium
//!
//! Environment variables:
//!   OPENAGENT_SOCKET_PATH   — Unix socket (default: data/sockets/browser.sock)
//!   BROWSER_BIN             — agent-browser binary (default: agent-browser)
//!   BROWSER_ARTIFACTS_DIR   — screenshot root (default: data/artifacts/browser)
//!
//! # Abort
//!
//! Panics if the log-level env filter directive is invalid, or if the session
//! mutex is poisoned due to a prior panic in a tool handler.

use anyhow::{Context, Result};
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;
use sdk_rust::{setup_otel, McpLiteServer, ToolDefinition};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};
use uuid::Uuid;

const DEFAULT_SOCKET_PATH: &str = "data/sockets/browser.sock";
const DEFAULT_BROWSER_BIN: &str = "agent-browser";
const DEFAULT_ARTIFACTS_DIR: &str = "data/artifacts/browser";

// ---------------------------------------------------------------------------
// Session registry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct BrowserSession {
    session_id: String,
    screenshot_dir: PathBuf,
    current_url: String,
}

type SessionMap = Arc<Mutex<HashMap<String, BrowserSession>>>;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn browser_bin() -> String {
    env::var("BROWSER_BIN").unwrap_or_else(|_| DEFAULT_BROWSER_BIN.to_string())
}

fn artifacts_dir() -> PathBuf {
    PathBuf::from(
        env::var("BROWSER_ARTIFACTS_DIR").unwrap_or_else(|_| DEFAULT_ARTIFACTS_DIR.to_string()),
    )
}

fn new_session_id() -> String {
    Uuid::new_v4().to_string().replace('-', "")[..12].to_string()
}

fn screenshot_path(dir: &Path) -> PathBuf {
    dir.join("latest.png")
}

fn ts_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Run: agent-browser --session <id> [args...]
/// Returns trimmed stdout (or stderr on failure).
fn run_session(session_id: &str, args: &[&str]) -> Result<String> {
    let bin = browser_bin();
    let output = Command::new(&bin)
        .arg("--session")
        .arg(session_id)
        .args(args)
        .output()
        .with_context(|| {
            format!(
                "Failed to execute '{}'. Install with: npm install -g agent-browser",
                bin
            )
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if !output.status.success() {
        let msg = if !stderr.is_empty() { &stderr } else { &stdout };
        return Err(anyhow::anyhow!(
            "agent-browser exited {}: {}",
            output.status,
            msg
        ));
    }

    Ok(if stdout.is_empty() { stderr } else { stdout })
}

/// Run command then take a screenshot. Returns the screenshot path.
fn run_with_screenshot(session_id: &str, cmd_args: &[&str], dir: &Path) -> Result<(String, PathBuf)> {
    let out = run_session(session_id, cmd_args)?;
    let ss = screenshot_path(dir);
    let ss_str = ss.to_string_lossy().to_string();
    run_session(session_id, &["screenshot", &ss_str])?;
    Ok((out, ss))
}

fn ensure_session_dir(session_id: &str) -> Result<PathBuf> {
    let dir = artifacts_dir().join(session_id);
    fs::create_dir_all(&dir)
        .with_context(|| format!("Cannot create screenshot dir {:?}", dir))?;
    Ok(dir)
}

fn get_or_create_session(
    sessions: &SessionMap,
    session_id: &str,
    url: &str,
) -> Result<BrowserSession> {
    let dir = ensure_session_dir(session_id)?;
    let s = BrowserSession {
        session_id: session_id.to_string(),
        screenshot_dir: dir,
        current_url: url.to_string(),
    };
    sessions.lock().unwrap().insert(session_id.to_string(), s.clone());
    Ok(s)
}

fn lookup_session(sessions: &SessionMap, session_id: &str) -> Result<BrowserSession> {
    sessions
        .lock()
        .unwrap()
        .get(session_id)
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Unknown session '{}'. Call browser.open first.",
                session_id
            )
        })
}

fn require_session_id(params: &Value) -> Result<String> {
    params
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Missing 'session_id'"))
}

fn require_str<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("Missing or empty '{}'", key))
}

fn ok_with_screenshot(session_id: &str, dir: &Path, extra: Value) -> Value {
    let ss = screenshot_path(dir);
    let mut base = json!({
        "ok": true,
        "session_id": session_id,
        "screenshot": ss.to_string_lossy(),
        "screenshot_ts": ts_ms(),
    });
    if let (Some(obj), Some(ext)) = (base.as_object_mut(), extra.as_object()) {
        for (k, v) in ext {
            obj.insert(k.clone(), v.clone());
        }
    }
    base
}

// ---------------------------------------------------------------------------
// Tool handlers
// ---------------------------------------------------------------------------

fn handle_open(params: Value, sessions: SessionMap) -> Result<String> {
    let url = require_str(&params, "url")?.trim().to_string();
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(new_session_id);

    let session = get_or_create_session(&sessions, &session_id, &url)?;
    let ss = screenshot_path(&session.screenshot_dir);
    let ss_str = ss.to_string_lossy().to_string();

    run_session(&session_id, &["open", &url])?;
    run_session(&session_id, &["screenshot", &ss_str])?;

    info!(session_id = %session_id, url = %url, "browser session opened");

    Ok(serde_json::to_string(&json!({
        "ok": true,
        "session_id": session_id,
        "url": url,
        "screenshot": ss_str,
        "screenshot_ts": ts_ms(),
    }))?)
}

fn handle_navigate(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let url = require_str(&params, "url")?.to_string();
    let session = lookup_session(&sessions, &session_id)?;

    let ss = screenshot_path(&session.screenshot_dir);
    let ss_str = ss.to_string_lossy().to_string();
    run_session(&session_id, &["open", &url])?;
    run_session(&session_id, &["screenshot", &ss_str])?;

    sessions.lock().unwrap().get_mut(&session_id).map(|s| s.current_url = url.clone());

    Ok(serde_json::to_string(&ok_with_screenshot(&session_id, &session.screenshot_dir, json!({ "url": url })))?)
}

fn handle_snapshot(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let interactive_only = params.get("interactive_only").and_then(|v| v.as_bool()).unwrap_or(false);
    let session = lookup_session(&sessions, &session_id)?;

    let ss = screenshot_path(&session.screenshot_dir);
    let ss_str = ss.to_string_lossy().to_string();

    let snap_args: &[&str] = if interactive_only { &["snapshot", "-i"] } else { &["snapshot"] };
    let text = run_session(&session_id, snap_args)?;
    run_session(&session_id, &["screenshot", &ss_str])?;

    Ok(serde_json::to_string(&json!({
        "ok": true,
        "session_id": session_id,
        "text": text,
        "screenshot": ss_str,
        "screenshot_ts": ts_ms(),
    }))?)
}

fn handle_screenshot(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let full_page = params.get("full_page").and_then(|v| v.as_bool()).unwrap_or(false);
    let session = lookup_session(&sessions, &session_id)?;

    let ss = screenshot_path(&session.screenshot_dir);
    let ss_str = ss.to_string_lossy().to_string();

    if full_page {
        run_session(&session_id, &["screenshot", &ss_str, "--full"])?;
    } else {
        run_session(&session_id, &["screenshot", &ss_str])?;
    }

    Ok(serde_json::to_string(&ok_with_screenshot(&session_id, &session.screenshot_dir, json!({})))?)
}

fn handle_click(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let session = lookup_session(&sessions, &session_id)?;

    // selector, ref (@e1), or x+y coordinates
    let selector = params.get("selector").and_then(|v| v.as_str()).map(|s| s.to_string());
    let new_tab = params.get("new_tab").and_then(|v| v.as_bool()).unwrap_or(false);

    if let Some(sel) = selector {
        if new_tab {
            run_session(&session_id, &["click", &sel, "--new-tab"])?;
        } else {
            run_session(&session_id, &["click", &sel])?;
        }
    } else {
        let x = params.get("x").and_then(|v| v.as_f64())
            .ok_or_else(|| anyhow::anyhow!("Provide 'selector' or 'x'+'y' coordinates"))?;
        let y = params.get("y").and_then(|v| v.as_f64())
            .ok_or_else(|| anyhow::anyhow!("Provide 'selector' or 'x'+'y' coordinates"))?;
        run_session(&session_id, &[
            "mouse", "move", &x.to_string(), &y.to_string(),
        ])?;
        run_session(&session_id, &["mouse", "down"])?;
        run_session(&session_id, &["mouse", "up"])?;
    }

    let ss = screenshot_path(&session.screenshot_dir);
    let ss_str = ss.to_string_lossy().to_string();
    run_session(&session_id, &["screenshot", &ss_str])?;

    Ok(serde_json::to_string(&ok_with_screenshot(&session_id, &session.screenshot_dir, json!({})))?)
}

fn handle_dblclick(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let selector = require_str(&params, "selector")?.to_string();
    let session = lookup_session(&sessions, &session_id)?;

    run_session(&session_id, &["dblclick", &selector])?;

    let ss = screenshot_path(&session.screenshot_dir);
    let ss_str = ss.to_string_lossy().to_string();
    run_session(&session_id, &["screenshot", &ss_str])?;

    Ok(serde_json::to_string(&ok_with_screenshot(&session_id, &session.screenshot_dir, json!({})))?)
}

fn handle_fill(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let selector = require_str(&params, "selector")?.to_string();
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let session = lookup_session(&sessions, &session_id)?;

    run_session(&session_id, &["fill", &selector, &text])?;

    let ss = screenshot_path(&session.screenshot_dir);
    let ss_str = ss.to_string_lossy().to_string();
    run_session(&session_id, &["screenshot", &ss_str])?;

    Ok(serde_json::to_string(&ok_with_screenshot(&session_id, &session.screenshot_dir, json!({})))?)
}

fn handle_type(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let text = require_str(&params, "text")?.to_string();
    let session = lookup_session(&sessions, &session_id)?;

    if let Some(sel) = params.get("selector").and_then(|v| v.as_str()) {
        run_session(&session_id, &["type", sel, &text])?;
    } else {
        run_session(&session_id, &["keyboard", "type", &text])?;
    }

    let ss = screenshot_path(&session.screenshot_dir);
    let ss_str = ss.to_string_lossy().to_string();
    run_session(&session_id, &["screenshot", &ss_str])?;

    Ok(serde_json::to_string(&ok_with_screenshot(&session_id, &session.screenshot_dir, json!({})))?)
}

fn handle_press(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let key = require_str(&params, "key")?.to_string();
    let session = lookup_session(&sessions, &session_id)?;

    run_session(&session_id, &["press", &key])?;

    let ss = screenshot_path(&session.screenshot_dir);
    let ss_str = ss.to_string_lossy().to_string();
    run_session(&session_id, &["screenshot", &ss_str])?;

    Ok(serde_json::to_string(&ok_with_screenshot(&session_id, &session.screenshot_dir, json!({})))?)
}

fn handle_hover(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let selector = require_str(&params, "selector")?.to_string();
    let session = lookup_session(&sessions, &session_id)?;

    run_session(&session_id, &["hover", &selector])?;

    let ss = screenshot_path(&session.screenshot_dir);
    let ss_str = ss.to_string_lossy().to_string();
    run_session(&session_id, &["screenshot", &ss_str])?;

    Ok(serde_json::to_string(&ok_with_screenshot(&session_id, &session.screenshot_dir, json!({})))?)
}

fn handle_select(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let selector = require_str(&params, "selector")?.to_string();
    let value = require_str(&params, "value")?.to_string();
    let session = lookup_session(&sessions, &session_id)?;

    run_session(&session_id, &["select", &selector, &value])?;

    let ss = screenshot_path(&session.screenshot_dir);
    let ss_str = ss.to_string_lossy().to_string();
    run_session(&session_id, &["screenshot", &ss_str])?;

    Ok(serde_json::to_string(&ok_with_screenshot(&session_id, &session.screenshot_dir, json!({})))?)
}

fn handle_check(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let selector = require_str(&params, "selector")?.to_string();
    let uncheck = params.get("uncheck").and_then(|v| v.as_bool()).unwrap_or(false);
    let session = lookup_session(&sessions, &session_id)?;

    if uncheck {
        run_session(&session_id, &["uncheck", &selector])?;
    } else {
        run_session(&session_id, &["check", &selector])?;
    }

    let ss = screenshot_path(&session.screenshot_dir);
    let ss_str = ss.to_string_lossy().to_string();
    run_session(&session_id, &["screenshot", &ss_str])?;

    Ok(serde_json::to_string(&ok_with_screenshot(&session_id, &session.screenshot_dir, json!({})))?)
}

fn handle_scroll(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let direction = params.get("direction").and_then(|v| v.as_str()).unwrap_or("down").to_string();
    let amount = params.get("amount").and_then(|v| v.as_i64()).unwrap_or(500).to_string();
    let session = lookup_session(&sessions, &session_id)?;

    if let Some(sel) = params.get("selector").and_then(|v| v.as_str()) {
        run_session(&session_id, &["scroll", &direction, &amount, "--selector", sel])?;
    } else {
        run_session(&session_id, &["scroll", &direction, &amount])?;
    }

    let ss = screenshot_path(&session.screenshot_dir);
    let ss_str = ss.to_string_lossy().to_string();
    run_session(&session_id, &["screenshot", &ss_str])?;

    Ok(serde_json::to_string(&ok_with_screenshot(&session_id, &session.screenshot_dir, json!({})))?)
}

fn handle_find(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let by = params.get("by").and_then(|v| v.as_str()).unwrap_or("text").to_string();
    let value = require_str(&params, "value")?.to_string();
    let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("click").to_string();
    let session = lookup_session(&sessions, &session_id)?;

    // Build: find [nth <n>] <by> <value> <action> [action_value] [--exact] [--name <name>]
    let mut args: Vec<String> = vec!["find".to_string()];
    if by == "nth" {
        let n = params.get("n").and_then(|v| v.as_i64())
            .ok_or_else(|| anyhow::anyhow!("'n' is required when by=nth"))?;
        args.extend(["nth".to_string(), n.to_string(), value.clone(), action.clone()]);
    } else {
        args.extend([by.clone(), value.clone(), action.clone()]);
    }
    if let Some(av) = params.get("action_value").and_then(|v| v.as_str()) {
        args.push(av.to_string());
    }
    if params.get("exact").and_then(|v| v.as_bool()).unwrap_or(false) {
        args.push("--exact".to_string());
    }
    if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
        args.extend(["--name".to_string(), name.to_string()]);
    }

    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    run_session(&session_id, &refs)?;

    let ss = screenshot_path(&session.screenshot_dir);
    let ss_str = ss.to_string_lossy().to_string();
    run_session(&session_id, &["screenshot", &ss_str])?;

    Ok(serde_json::to_string(&ok_with_screenshot(&session_id, &session.screenshot_dir, json!({})))?)
}

fn handle_get(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let what = params.get("what").and_then(|v| v.as_str()).unwrap_or("text").to_string();
    lookup_session(&sessions, &session_id)?;

    let result = if let Some(sel) = params.get("selector").and_then(|v| v.as_str()) {
        if what == "attr" {
            let attr = params.get("attr").and_then(|v| v.as_str()).unwrap_or("href");
            run_session(&session_id, &["get", &what, sel, attr])?
        } else {
            run_session(&session_id, &["get", &what, sel])?
        }
    } else {
        // title, url
        run_session(&session_id, &["get", &what])?
    };

    Ok(serde_json::to_string(&json!({
        "ok": true,
        "session_id": session_id,
        "what": what,
        "result": result,
    }))?)
}

fn handle_wait(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    lookup_session(&sessions, &session_id)?;

    // selector, text pattern, url pattern, ms, or load state
    if let Some(ms) = params.get("ms").and_then(|v| v.as_i64()) {
        run_session(&session_id, &["wait", &ms.to_string()])?;
    } else if let Some(text) = params.get("text").and_then(|v| v.as_str()) {
        run_session(&session_id, &["wait", "--text", text])?;
    } else if let Some(url_pattern) = params.get("url_pattern").and_then(|v| v.as_str()) {
        run_session(&session_id, &["wait", "--url", url_pattern])?;
    } else if let Some(load) = params.get("load_state").and_then(|v| v.as_str()) {
        run_session(&session_id, &["wait", "--load", load])?;
    } else {
        let sel = require_str(&params, "selector")?.to_string();
        run_session(&session_id, &["wait", &sel])?;
    }

    Ok(serde_json::to_string(&json!({ "ok": true, "session_id": session_id }))?)
}

fn handle_eval(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let js = require_str(&params, "js")?.to_string();
    lookup_session(&sessions, &session_id)?;

    let result = run_session(&session_id, &["eval", &js])?;

    Ok(serde_json::to_string(&json!({
        "ok": true,
        "session_id": session_id,
        "result": result,
    }))?)
}

fn handle_extract(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    lookup_session(&sessions, &session_id)?;

    let result = if let Some(sel) = params.get("selector").and_then(|v| v.as_str()) {
        run_session(&session_id, &["get", "text", sel])?
    } else {
        // Get full page text via snapshot
        run_session(&session_id, &["snapshot"])?
    };

    Ok(serde_json::to_string(&json!({
        "ok": true,
        "session_id": session_id,
        "text": result,
    }))?)
}

fn handle_tab_new(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    lookup_session(&sessions, &session_id)?;

    if let Some(url) = params.get("url").and_then(|v| v.as_str()) {
        run_session(&session_id, &["tab", "new", url])?;
    } else {
        run_session(&session_id, &["tab", "new"])?;
    }

    let tab_list = run_session(&session_id, &["tab"])?;

    Ok(serde_json::to_string(&json!({
        "ok": true,
        "session_id": session_id,
        "tabs": tab_list,
    }))?)
}

fn handle_tab_switch(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let n = params.get("n").and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow::anyhow!("Missing 'n' (tab number)"))?;
    lookup_session(&sessions, &session_id)?;

    run_session(&session_id, &["tab", &n.to_string()])?;

    let current_url = run_session(&session_id, &["get", "url"]).unwrap_or_default();
    sessions.lock().unwrap().get_mut(&session_id).map(|s| s.current_url = current_url.clone());

    Ok(serde_json::to_string(&json!({
        "ok": true,
        "session_id": session_id,
        "tab": n,
        "url": current_url,
    }))?)
}

fn handle_scrollinto(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let selector = require_str(&params, "selector")?.to_string();
    let session = lookup_session(&sessions, &session_id)?;

    run_session(&session_id, &["scrollintoview", &selector])?;

    let ss = screenshot_path(&session.screenshot_dir);
    let ss_str = ss.to_string_lossy().to_string();
    run_session(&session_id, &["screenshot", &ss_str])?;

    Ok(serde_json::to_string(&ok_with_screenshot(&session_id, &session.screenshot_dir, json!({})))?)
}

fn handle_cookies(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    lookup_session(&sessions, &session_id)?;

    let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("get");

    let result = match action {
        "clear" => {
            run_session(&session_id, &["cookies", "clear"])?
        }
        "set" => {
            let name = require_str(&params, "name")?.to_string();
            let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string();
            run_session(&session_id, &["cookies", "set", &name, &value])?
        }
        _ => run_session(&session_id, &["cookies"])?,
    };

    Ok(serde_json::to_string(&json!({
        "ok": true,
        "session_id": session_id,
        "result": result,
    }))?)
}

fn handle_state(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    lookup_session(&sessions, &session_id)?;

    let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("save");
    let state_dir = artifacts_dir().join(&session_id).join("state.json");
    let state_str = state_dir.to_string_lossy().to_string();

    let result = match action {
        "load" => run_session(&session_id, &["state", "load", &state_str])?,
        _ => run_session(&session_id, &["state", "save", &state_str])?,
    };

    Ok(serde_json::to_string(&json!({
        "ok": true,
        "session_id": session_id,
        "action": action,
        "state_file": state_str,
        "result": result,
    }))?)
}

fn handle_pdf(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let session = lookup_session(&sessions, &session_id)?;

    let pdf_path = session.screenshot_dir.join("page.pdf");
    let pdf_str = pdf_path.to_string_lossy().to_string();
    run_session(&session_id, &["pdf", &pdf_str])?;

    Ok(serde_json::to_string(&json!({
        "ok": true,
        "session_id": session_id,
        "pdf": pdf_str,
    }))?)
}

fn handle_diff(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let kind = params.get("kind").and_then(|v| v.as_str()).unwrap_or("snapshot");
    lookup_session(&sessions, &session_id)?;

    let result = match kind {
        "screenshot" => {
            let baseline = require_str(&params, "baseline")?.to_string();
            run_session(&session_id, &["diff", "screenshot", "--baseline", &baseline])?
        }
        _ => run_session(&session_id, &["diff", "snapshot"])?,
    };

    Ok(serde_json::to_string(&json!({
        "ok": true,
        "session_id": session_id,
        "diff": result,
    }))?)
}

fn handle_close(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    {
        let mut guard = sessions.lock().unwrap();
        guard.remove(&session_id).ok_or_else(|| anyhow::anyhow!("Unknown session '{}'", session_id))?;
    }
    let _ = run_session(&session_id, &["close"]);
    warn!(session_id = %session_id, "browser session closed");
    Ok(serde_json::to_string(&json!({ "ok": true, "session_id": session_id, "closed": true }))?)
}

// ---------------------------------------------------------------------------
// New handlers — navigation, input, frames, tabs, storage, settings, network
// ---------------------------------------------------------------------------

fn handle_back(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let session = lookup_session(&sessions, &session_id)?;
    run_session(&session_id, &["back"])?;
    let ss = screenshot_path(&session.screenshot_dir);
    run_session(&session_id, &["screenshot", &ss.to_string_lossy().to_string()])?;
    Ok(serde_json::to_string(&ok_with_screenshot(&session_id, &session.screenshot_dir, json!({})))?)
}

fn handle_forward(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let session = lookup_session(&sessions, &session_id)?;
    run_session(&session_id, &["forward"])?;
    let ss = screenshot_path(&session.screenshot_dir);
    run_session(&session_id, &["screenshot", &ss.to_string_lossy().to_string()])?;
    Ok(serde_json::to_string(&ok_with_screenshot(&session_id, &session.screenshot_dir, json!({})))?)
}

fn handle_reload(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let session = lookup_session(&sessions, &session_id)?;
    run_session(&session_id, &["reload"])?;
    let ss = screenshot_path(&session.screenshot_dir);
    run_session(&session_id, &["screenshot", &ss.to_string_lossy().to_string()])?;
    Ok(serde_json::to_string(&ok_with_screenshot(&session_id, &session.screenshot_dir, json!({})))?)
}

fn handle_focus(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let selector = require_str(&params, "selector")?.to_string();
    lookup_session(&sessions, &session_id)?;
    run_session(&session_id, &["focus", &selector])?;
    Ok(serde_json::to_string(&json!({ "ok": true, "session_id": session_id }))?)
}

fn handle_drag(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let source = require_str(&params, "source")?.to_string();
    let target = require_str(&params, "target")?.to_string();
    let session = lookup_session(&sessions, &session_id)?;
    run_session(&session_id, &["drag", &source, &target])?;
    let ss = screenshot_path(&session.screenshot_dir);
    run_session(&session_id, &["screenshot", &ss.to_string_lossy().to_string()])?;
    Ok(serde_json::to_string(&ok_with_screenshot(&session_id, &session.screenshot_dir, json!({})))?)
}

fn handle_upload(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let selector = require_str(&params, "selector")?.to_string();
    let file = require_str(&params, "file")?.to_string();
    let session = lookup_session(&sessions, &session_id)?;
    run_session(&session_id, &["upload", &selector, &file])?;
    let ss = screenshot_path(&session.screenshot_dir);
    run_session(&session_id, &["screenshot", &ss.to_string_lossy().to_string()])?;
    Ok(serde_json::to_string(&ok_with_screenshot(&session_id, &session.screenshot_dir, json!({})))?)
}

fn handle_keydown(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let key = require_str(&params, "key")?.to_string();
    lookup_session(&sessions, &session_id)?;
    run_session(&session_id, &["keydown", &key])?;
    Ok(serde_json::to_string(&json!({ "ok": true, "session_id": session_id }))?)
}

fn handle_keyup(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let key = require_str(&params, "key")?.to_string();
    lookup_session(&sessions, &session_id)?;
    run_session(&session_id, &["keyup", &key])?;
    Ok(serde_json::to_string(&json!({ "ok": true, "session_id": session_id }))?)
}

fn handle_frame(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    lookup_session(&sessions, &session_id)?;
    let selector = params.get("selector").and_then(|v| v.as_str()).unwrap_or("main");
    if selector == "main" || selector.is_empty() {
        run_session(&session_id, &["frame", "main"])?;
    } else {
        run_session(&session_id, &["frame", selector])?;
    }
    Ok(serde_json::to_string(&json!({ "ok": true, "session_id": session_id }))?)
}

fn handle_dialog(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    lookup_session(&sessions, &session_id)?;
    let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("dismiss");
    let result = if action == "accept" {
        if let Some(text) = params.get("text").and_then(|v| v.as_str()) {
            run_session(&session_id, &["dialog", "accept", text])?
        } else {
            run_session(&session_id, &["dialog", "accept"])?
        }
    } else {
        run_session(&session_id, &["dialog", "dismiss"])?
    };
    Ok(serde_json::to_string(&json!({ "ok": true, "session_id": session_id, "result": result }))?)
}

fn handle_is(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let check = params.get("check").and_then(|v| v.as_str()).unwrap_or("visible").to_string();
    let selector = require_str(&params, "selector")?.to_string();
    lookup_session(&sessions, &session_id)?;
    let result = run_session(&session_id, &["is", &check, &selector]).unwrap_or_default();
    let value = result.trim().eq_ignore_ascii_case("true") || result.trim() == "1";
    Ok(serde_json::to_string(&json!({ "ok": true, "session_id": session_id, "check": check, "result": value }))?)
}

fn handle_tab_list(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    lookup_session(&sessions, &session_id)?;
    let result = run_session(&session_id, &["tab"])?;
    Ok(serde_json::to_string(&json!({ "ok": true, "session_id": session_id, "tabs": result }))?)
}

fn handle_tab_close(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    lookup_session(&sessions, &session_id)?;
    if let Some(n) = params.get("n").and_then(|v| v.as_i64()) {
        run_session(&session_id, &["tab", "close", &n.to_string()])?;
    } else {
        run_session(&session_id, &["tab", "close"])?;
    }
    Ok(serde_json::to_string(&json!({ "ok": true, "session_id": session_id }))?)
}

fn handle_storage(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    lookup_session(&sessions, &session_id)?;
    let store = params.get("store").and_then(|v| v.as_str()).unwrap_or("local");
    let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("get");
    let result = match action {
        "set" => {
            let key = require_str(&params, "key")?.to_string();
            let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string();
            run_session(&session_id, &["storage", store, "set", &key, &value])?
        }
        "clear" => run_session(&session_id, &["storage", store, "clear"])?,
        _ => {
            if let Some(key) = params.get("key").and_then(|v| v.as_str()) {
                run_session(&session_id, &["storage", store, key])?
            } else {
                run_session(&session_id, &["storage", store])?
            }
        }
    };
    Ok(serde_json::to_string(&json!({ "ok": true, "session_id": session_id, "result": result }))?)
}

fn handle_set(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    lookup_session(&sessions, &session_id)?;
    let what = params.get("what").and_then(|v| v.as_str()).unwrap_or("");
    let result = match what {
        "viewport" => {
            let w = params.get("width").and_then(|v| v.as_i64()).unwrap_or(1280).to_string();
            let h = params.get("height").and_then(|v| v.as_i64()).unwrap_or(800).to_string();
            run_session(&session_id, &["set", "viewport", &w, &h])?
        }
        "device" => {
            let name = require_str(&params, "name")?.to_string();
            run_session(&session_id, &["set", "device", &name])?
        }
        "geo" => {
            let lat = params.get("lat").and_then(|v| v.as_f64()).unwrap_or(0.0).to_string();
            let lng = params.get("lng").and_then(|v| v.as_f64()).unwrap_or(0.0).to_string();
            run_session(&session_id, &["set", "geo", &lat, &lng])?
        }
        "offline" => {
            let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("on");
            run_session(&session_id, &["set", "offline", value])?
        }
        "headers" => {
            let json_str = require_str(&params, "json")?.to_string();
            run_session(&session_id, &["set", "headers", &json_str])?
        }
        "credentials" => {
            let username = require_str(&params, "username")?.to_string();
            let password = require_str(&params, "password")?.to_string();
            run_session(&session_id, &["set", "credentials", &username, &password])?
        }
        "media" => {
            let scheme = params.get("scheme").and_then(|v| v.as_str()).unwrap_or("dark");
            run_session(&session_id, &["set", "media", scheme])?
        }
        _ => return Err(anyhow::anyhow!(
            "Unknown 'what': {}. Use: viewport, device, geo, offline, headers, credentials, media", what
        )),
    };
    Ok(serde_json::to_string(&json!({ "ok": true, "session_id": session_id, "result": result }))?)
}

fn handle_network(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    lookup_session(&sessions, &session_id)?;
    let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("requests");
    let result = match action {
        "route" => {
            let url_pattern = require_str(&params, "url_pattern")?.to_string();
            if params.get("abort").and_then(|v| v.as_bool()).unwrap_or(false) {
                run_session(&session_id, &["network", "route", &url_pattern, "--abort"])?
            } else if let Some(body) = params.get("body").and_then(|v| v.as_str()) {
                run_session(&session_id, &["network", "route", &url_pattern, "--body", body])?
            } else {
                run_session(&session_id, &["network", "route", &url_pattern])?
            }
        }
        "unroute" => {
            if let Some(url_pattern) = params.get("url_pattern").and_then(|v| v.as_str()) {
                run_session(&session_id, &["network", "unroute", url_pattern])?
            } else {
                run_session(&session_id, &["network", "unroute"])?
            }
        }
        _ => {
            if let Some(filter) = params.get("filter").and_then(|v| v.as_str()) {
                run_session(&session_id, &["network", "requests", "--filter", filter])?
            } else {
                run_session(&session_id, &["network", "requests"])?
            }
        }
    };
    Ok(serde_json::to_string(&json!({ "ok": true, "session_id": session_id, "result": result }))?)
}

fn handle_mouse(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    lookup_session(&sessions, &session_id)?;
    let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("move");
    let result = match action {
        "move" => {
            let x = params.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0).to_string();
            let y = params.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0).to_string();
            run_session(&session_id, &["mouse", "move", &x, &y])?
        }
        "down" => {
            let button = params.get("button").and_then(|v| v.as_str()).unwrap_or("left");
            run_session(&session_id, &["mouse", "down", button])?
        }
        "up" => {
            let button = params.get("button").and_then(|v| v.as_str()).unwrap_or("left");
            run_session(&session_id, &["mouse", "up", button])?
        }
        "wheel" => {
            let dy = params.get("dy").and_then(|v| v.as_f64()).unwrap_or(0.0).to_string();
            let dx = params.get("dx").and_then(|v| v.as_f64()).unwrap_or(0.0).to_string();
            run_session(&session_id, &["mouse", "wheel", &dy, &dx])?
        }
        _ => return Err(anyhow::anyhow!("Unknown mouse action: {}. Use: move, down, up, wheel", action)),
    };
    Ok(serde_json::to_string(&json!({ "ok": true, "session_id": session_id, "result": result }))?)
}

fn handle_console(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    lookup_session(&sessions, &session_id)?;
    let result = if params.get("clear").and_then(|v| v.as_bool()).unwrap_or(false) {
        run_session(&session_id, &["console", "--clear"])?
    } else {
        run_session(&session_id, &["console"])?
    };
    Ok(serde_json::to_string(&json!({ "ok": true, "session_id": session_id, "output": result }))?)
}

fn handle_errors(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    lookup_session(&sessions, &session_id)?;
    let result = if params.get("clear").and_then(|v| v.as_bool()).unwrap_or(false) {
        run_session(&session_id, &["errors", "--clear"])?
    } else {
        run_session(&session_id, &["errors"])?
    };
    Ok(serde_json::to_string(&json!({ "ok": true, "session_id": session_id, "errors": result }))?)
}

fn handle_highlight(params: Value, sessions: SessionMap) -> Result<String> {
    let session_id = require_session_id(&params)?;
    let selector = require_str(&params, "selector")?.to_string();
    let session = lookup_session(&sessions, &session_id)?;
    run_session(&session_id, &["highlight", &selector])?;
    let ss = screenshot_path(&session.screenshot_dir);
    run_session(&session_id, &["screenshot", &ss.to_string_lossy().to_string()])?;
    Ok(serde_json::to_string(&ok_with_screenshot(&session_id, &session.screenshot_dir, json!({})))?)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let logs_dir = env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| "logs".to_string());
    // _otel_guard must be kept alive until end of main — drop flushes spans.
    let _otel_guard = match setup_otel("browser", &logs_dir) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("otel init failed (continuing without file traces): {e}");
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::from_default_env()
                        .add_directive("browser=info".parse().unwrap()),
                )
                .try_init()
                .ok();
            None
        }
    };

    let socket_path = env::var("OPENAGENT_SOCKET_PATH")
        .unwrap_or_else(|_| DEFAULT_SOCKET_PATH.to_string());

    fs::create_dir_all(artifacts_dir()).ok();

    let sessions: SessionMap = Arc::new(Mutex::new(HashMap::new()));

    macro_rules! handler {
        ($fn:ident, $sessions:expr) => {{
            let s = Arc::clone(&$sessions);
            move |p: Value| $fn(p, Arc::clone(&s))
        }};
    }

    let tools = vec![
        ToolDefinition { name: "browser.open".to_string(),
            description: "Open a URL in a new or existing named browser session. Each session has isolated cookies/storage. Returns session_id and screenshot path. Pass session_id to reuse an existing session.".to_string(),
            params: json!({ "type":"object","properties":{ "url":{"type":"string","description":"URL to open"},"session_id":{"type":"string","description":"Optional: reuse existing session"} },"required":["url"] }) },
        ToolDefinition { name: "browser.navigate".to_string(),
            description: "Navigate an existing session to a new URL.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"url":{"type":"string"} },"required":["session_id","url"] }) },
        ToolDefinition { name: "browser.snapshot".to_string(),
            description: "Get accessibility tree (AI-optimized page representation) + screenshot. Ref IDs like @e1, @e2 can be used in click/fill/type.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"interactive_only":{"type":"boolean","description":"Only include interactive elements (default false)"} },"required":["session_id"] }) },
        ToolDefinition { name: "browser.screenshot".to_string(),
            description: "Take a screenshot of the current page.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"full_page":{"type":"boolean","description":"Full page scroll screenshot (default false)"} },"required":["session_id"] }) },
        ToolDefinition { name: "browser.click".to_string(),
            description: "Click an element by CSS selector, accessibility ref (@e1), or pixel coordinates x+y.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"selector":{"type":"string","description":"CSS selector or @ref (e.g. @e2)"},"x":{"type":"number"},"y":{"type":"number"},"new_tab":{"type":"boolean"} },"required":["session_id"] }) },
        ToolDefinition { name: "browser.dblclick".to_string(),
            description: "Double-click an element by CSS selector or accessibility ref.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"selector":{"type":"string"} },"required":["session_id","selector"] }) },
        ToolDefinition { name: "browser.fill".to_string(),
            description: "Clear an input and fill with text. Preferred over type for form inputs.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"selector":{"type":"string","description":"CSS selector or @ref"},"text":{"type":"string"} },"required":["session_id","selector"] }) },
        ToolDefinition { name: "browser.type".to_string(),
            description: "Type text into a selector (appends to existing value) or use keyboard type if no selector.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"text":{"type":"string"},"selector":{"type":"string","description":"CSS selector or @ref (optional)"} },"required":["session_id","text"] }) },
        ToolDefinition { name: "browser.press".to_string(),
            description: "Press a key or key combination. Examples: Enter, Tab, Control+a, Escape, ArrowDown.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"key":{"type":"string","description":"Key name e.g. Enter, Tab, Control+a"} },"required":["session_id","key"] }) },
        ToolDefinition { name: "browser.hover".to_string(),
            description: "Hover over an element (triggers tooltips, dropdown menus, etc.).".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"selector":{"type":"string"} },"required":["session_id","selector"] }) },
        ToolDefinition { name: "browser.select".to_string(),
            description: "Select an option in a <select> dropdown.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"selector":{"type":"string","description":"CSS selector for <select>"},"value":{"type":"string","description":"Option value to select"} },"required":["session_id","selector","value"] }) },
        ToolDefinition { name: "browser.check".to_string(),
            description: "Check or uncheck a checkbox. Set uncheck=true to uncheck.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"selector":{"type":"string"},"uncheck":{"type":"boolean","description":"Set true to uncheck (default: check)"} },"required":["session_id","selector"] }) },
        ToolDefinition { name: "browser.scroll".to_string(),
            description: "Scroll the page or a specific element.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"direction":{"type":"string","enum":["up","down","left","right"]},"amount":{"type":"number","description":"Pixels (default 500)"},"selector":{"type":"string","description":"Scroll inside this element (optional)"} },"required":["session_id","direction"] }) },
        ToolDefinition { name: "browser.scrollinto".to_string(),
            description: "Scroll an element into view.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"selector":{"type":"string"} },"required":["session_id","selector"] }) },
        ToolDefinition { name: "browser.find".to_string(),
            description: "Find and interact with elements by semantic attributes: role, text, label, placeholder, alt, testid.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"by":{"type":"string","enum":["role","text","label","placeholder","alt","testid","title"],"description":"How to find element"},"value":{"type":"string","description":"Value to match"},"action":{"type":"string","description":"Action: click, fill, check (default: click)"},"action_value":{"type":"string","description":"Value for fill action"} },"required":["session_id","by","value"] }) },
        ToolDefinition { name: "browser.get".to_string(),
            description: "Get data from the page: text, html, value, attr, title, url, count, box.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"what":{"type":"string","enum":["text","html","value","attr","title","url","count","box","styles"],"description":"What to get"},"selector":{"type":"string","description":"CSS selector (required for text/html/value/attr/count/box)"},"attr":{"type":"string","description":"Attribute name (for what=attr)"} },"required":["session_id","what"] }) },
        ToolDefinition { name: "browser.wait".to_string(),
            description: "Wait for a condition: element visible, text to appear, URL pattern, load state, or a delay in ms.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"selector":{"type":"string","description":"Wait for element to be visible"},"text":{"type":"string","description":"Wait for text to appear"},"url_pattern":{"type":"string","description":"Wait for URL to match pattern"},"load_state":{"type":"string","enum":["load","domcontentloaded","networkidle"]},"ms":{"type":"number","description":"Wait N milliseconds"} },"required":["session_id"] }) },
        ToolDefinition { name: "browser.eval".to_string(),
            description: "Evaluate JavaScript in the page context and return the result.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"js":{"type":"string","description":"JavaScript expression to evaluate"} },"required":["session_id","js"] }) },
        ToolDefinition { name: "browser.extract".to_string(),
            description: "Extract readable text content from the page or a CSS-scoped element.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"selector":{"type":"string","description":"Scope extraction to this CSS selector (optional)"} },"required":["session_id"] }) },
        ToolDefinition { name: "browser.tab_new".to_string(),
            description: "Open a new tab in this session, optionally navigating to a URL.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"url":{"type":"string"} },"required":["session_id"] }) },
        ToolDefinition { name: "browser.tab_switch".to_string(),
            description: "Switch to tab N (1-indexed) in this session.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"n":{"type":"number","description":"Tab number (1-indexed)"} },"required":["session_id","n"] }) },
        ToolDefinition { name: "browser.cookies".to_string(),
            description: "Get, set, or clear cookies for this session.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"action":{"type":"string","enum":["get","set","clear"],"description":"Action (default: get)"},"name":{"type":"string","description":"Cookie name (for set)"},"value":{"type":"string","description":"Cookie value (for set)"} },"required":["session_id"] }) },
        ToolDefinition { name: "browser.state".to_string(),
            description: "Save or load auth state (cookies, localStorage) to/from disk for this session.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"action":{"type":"string","enum":["save","load"],"description":"save or load (default: save)"} },"required":["session_id"] }) },
        ToolDefinition { name: "browser.pdf".to_string(),
            description: "Save the current page as a PDF to the session artifacts directory.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"} },"required":["session_id"] }) },
        ToolDefinition { name: "browser.diff".to_string(),
            description: "Diff current page snapshot or screenshot against the previous or a baseline file.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"kind":{"type":"string","enum":["snapshot","screenshot"],"description":"What to diff (default: snapshot)"},"baseline":{"type":"string","description":"Baseline file path (for screenshot diff)"} },"required":["session_id"] }) },
        ToolDefinition { name: "browser.close".to_string(),
            description: "Close this browser session and release resources.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"} },"required":["session_id"] }) },
        // --- navigation ---
        ToolDefinition { name: "browser.back".to_string(),
            description: "Navigate back to the previous page.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"} },"required":["session_id"] }) },
        ToolDefinition { name: "browser.forward".to_string(),
            description: "Navigate forward to the next page.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"} },"required":["session_id"] }) },
        ToolDefinition { name: "browser.reload".to_string(),
            description: "Reload the current page.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"} },"required":["session_id"] }) },
        // --- interactions ---
        ToolDefinition { name: "browser.focus".to_string(),
            description: "Focus an element without clicking it. Useful before keyboard shortcuts.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"selector":{"type":"string","description":"CSS selector or @ref"} },"required":["session_id","selector"] }) },
        ToolDefinition { name: "browser.drag".to_string(),
            description: "Drag an element from source to target (CSS selectors or @refs).".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"source":{"type":"string","description":"CSS selector or @ref to drag from"},"target":{"type":"string","description":"CSS selector or @ref to drop onto"} },"required":["session_id","source","target"] }) },
        ToolDefinition { name: "browser.upload".to_string(),
            description: "Upload a file to a file input element.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"selector":{"type":"string","description":"CSS selector or @ref for file input"},"file":{"type":"string","description":"Absolute path to the file to upload"} },"required":["session_id","selector","file"] }) },
        ToolDefinition { name: "browser.keydown".to_string(),
            description: "Hold a key down (pair with browser.keyup). Use for Shift+click, drag-with-modifier, etc.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"key":{"type":"string","description":"Key to hold: Shift, Control, Alt, Meta"} },"required":["session_id","key"] }) },
        ToolDefinition { name: "browser.keyup".to_string(),
            description: "Release a held key (use after browser.keydown).".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"key":{"type":"string","description":"Key to release: Shift, Control, Alt, Meta"} },"required":["session_id","key"] }) },
        // --- frames & dialogs ---
        ToolDefinition { name: "browser.frame".to_string(),
            description: "Switch into an iframe (selector) or back to the main frame (selector='main').".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"selector":{"type":"string","description":"CSS selector for iframe, or 'main' to return to top frame (default: main)"} },"required":["session_id"] }) },
        ToolDefinition { name: "browser.dialog".to_string(),
            description: "Accept or dismiss a browser alert/confirm/prompt dialog.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"action":{"type":"string","enum":["accept","dismiss"],"description":"accept or dismiss (default: dismiss)"},"text":{"type":"string","description":"Text to enter for prompt dialogs (accept only)"} },"required":["session_id"] }) },
        // --- state checks ---
        ToolDefinition { name: "browser.is".to_string(),
            description: "Check element state: visible, enabled, or checked. Returns boolean result.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"check":{"type":"string","enum":["visible","enabled","checked"],"description":"What to check (default: visible)"},"selector":{"type":"string","description":"CSS selector or @ref"} },"required":["session_id","selector"] }) },
        // --- tabs ---
        ToolDefinition { name: "browser.tab_list".to_string(),
            description: "List all open tabs in this session.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"} },"required":["session_id"] }) },
        ToolDefinition { name: "browser.tab_close".to_string(),
            description: "Close current tab or a specific tab by number (1-indexed).".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"n":{"type":"number","description":"Tab number to close (default: current tab)"} },"required":["session_id"] }) },
        // --- storage ---
        ToolDefinition { name: "browser.storage".to_string(),
            description: "Read or write localStorage/sessionStorage. Actions: get (all), get key, set key+value, clear.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"store":{"type":"string","enum":["local","session"],"description":"local=localStorage, session=sessionStorage (default: local)"},"action":{"type":"string","enum":["get","set","clear"],"description":"get, set, or clear (default: get)"},"key":{"type":"string","description":"Storage key (get specific key or set)"},"value":{"type":"string","description":"Value to store (for set)"} },"required":["session_id"] }) },
        // --- browser settings ---
        ToolDefinition { name: "browser.set".to_string(),
            description: "Configure browser settings. what=viewport(width,height), device(name), geo(lat,lng), offline(value=on/off), headers(json), credentials(username,password), media(scheme=dark/light).".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"what":{"type":"string","enum":["viewport","device","geo","offline","headers","credentials","media"],"description":"What setting to change"},"width":{"type":"number"},"height":{"type":"number"},"name":{"type":"string"},"lat":{"type":"number"},"lng":{"type":"number"},"value":{"type":"string"},"json":{"type":"string"},"username":{"type":"string"},"password":{"type":"string"},"scheme":{"type":"string","enum":["dark","light"]} },"required":["session_id","what"] }) },
        // --- network ---
        ToolDefinition { name: "browser.network".to_string(),
            description: "Intercept or inspect network requests. action=route(url_pattern, abort=true or body=json), unroute(url_pattern), requests(filter).".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"action":{"type":"string","enum":["route","unroute","requests"],"description":"route, unroute, or requests (default: requests)"},"url_pattern":{"type":"string","description":"URL glob pattern (for route/unroute)"},"abort":{"type":"boolean","description":"Block matching requests (route only)"},"body":{"type":"string","description":"Mock response JSON (route only)"},"filter":{"type":"string","description":"Filter pattern (requests only)"} },"required":["session_id"] }) },
        // --- mouse ---
        ToolDefinition { name: "browser.mouse".to_string(),
            description: "Low-level mouse control: move(x,y), down(button), up(button), wheel(dy,dx).".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"action":{"type":"string","enum":["move","down","up","wheel"],"description":"Mouse action"},"x":{"type":"number"},"y":{"type":"number"},"button":{"type":"string","enum":["left","right","middle"],"description":"Mouse button (default: left)"},"dy":{"type":"number","description":"Vertical wheel delta"},"dx":{"type":"number","description":"Horizontal wheel delta"} },"required":["session_id","action"] }) },
        // --- debugging ---
        ToolDefinition { name: "browser.console".to_string(),
            description: "View browser console messages (log, warn, error). Set clear=true to clear.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"clear":{"type":"boolean","description":"Clear console after reading (default: false)"} },"required":["session_id"] }) },
        ToolDefinition { name: "browser.errors".to_string(),
            description: "View uncaught JavaScript errors on the page. Set clear=true to clear.".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"clear":{"type":"boolean","description":"Clear errors after reading (default: false)"} },"required":["session_id"] }) },
        ToolDefinition { name: "browser.highlight".to_string(),
            description: "Visually highlight an element on the page (useful for verifying selectors).".to_string(),
            params: json!({ "type":"object","properties":{ "session_id":{"type":"string"},"selector":{"type":"string","description":"CSS selector or @ref"} },"required":["session_id","selector"] }) },
    ];

    let mut server = McpLiteServer::new(tools, "ready");

    server.register_tool("browser.open",       handler!(handle_open, sessions));
    server.register_tool("browser.navigate",   handler!(handle_navigate, sessions));
    server.register_tool("browser.snapshot",   handler!(handle_snapshot, sessions));
    server.register_tool("browser.screenshot", handler!(handle_screenshot, sessions));
    server.register_tool("browser.click",      handler!(handle_click, sessions));
    server.register_tool("browser.dblclick",   handler!(handle_dblclick, sessions));
    server.register_tool("browser.fill",       handler!(handle_fill, sessions));
    server.register_tool("browser.type",       handler!(handle_type, sessions));
    server.register_tool("browser.press",      handler!(handle_press, sessions));
    server.register_tool("browser.hover",      handler!(handle_hover, sessions));
    server.register_tool("browser.select",     handler!(handle_select, sessions));
    server.register_tool("browser.check",      handler!(handle_check, sessions));
    server.register_tool("browser.scroll",     handler!(handle_scroll, sessions));
    server.register_tool("browser.scrollinto", handler!(handle_scrollinto, sessions));
    server.register_tool("browser.find",       handler!(handle_find, sessions));
    server.register_tool("browser.get",        handler!(handle_get, sessions));
    server.register_tool("browser.wait",       handler!(handle_wait, sessions));
    server.register_tool("browser.eval",       handler!(handle_eval, sessions));
    server.register_tool("browser.extract",    handler!(handle_extract, sessions));
    server.register_tool("browser.tab_new",    handler!(handle_tab_new, sessions));
    server.register_tool("browser.tab_switch", handler!(handle_tab_switch, sessions));
    server.register_tool("browser.cookies",    handler!(handle_cookies, sessions));
    server.register_tool("browser.state",      handler!(handle_state, sessions));
    server.register_tool("browser.pdf",        handler!(handle_pdf, sessions));
    server.register_tool("browser.diff",       handler!(handle_diff, sessions));
    server.register_tool("browser.close",      handler!(handle_close, sessions));
    server.register_tool("browser.back",       handler!(handle_back, sessions));
    server.register_tool("browser.forward",    handler!(handle_forward, sessions));
    server.register_tool("browser.reload",     handler!(handle_reload, sessions));
    server.register_tool("browser.focus",      handler!(handle_focus, sessions));
    server.register_tool("browser.drag",       handler!(handle_drag, sessions));
    server.register_tool("browser.upload",     handler!(handle_upload, sessions));
    server.register_tool("browser.keydown",    handler!(handle_keydown, sessions));
    server.register_tool("browser.keyup",      handler!(handle_keyup, sessions));
    server.register_tool("browser.frame",      handler!(handle_frame, sessions));
    server.register_tool("browser.dialog",     handler!(handle_dialog, sessions));
    server.register_tool("browser.is",         handler!(handle_is, sessions));
    server.register_tool("browser.tab_list",   handler!(handle_tab_list, sessions));
    server.register_tool("browser.tab_close",  handler!(handle_tab_close, sessions));
    server.register_tool("browser.storage",    handler!(handle_storage, sessions));
    server.register_tool("browser.set",        handler!(handle_set, sessions));
    server.register_tool("browser.network",    handler!(handle_network, sessions));
    server.register_tool("browser.mouse",      handler!(handle_mouse, sessions));
    server.register_tool("browser.console",    handler!(handle_console, sessions));
    server.register_tool("browser.errors",     handler!(handle_errors, sessions));
    server.register_tool("browser.highlight",  handler!(handle_highlight, sessions));

    info!("Browser service starting on {}", socket_path);
    server.serve(&socket_path).await?;
    Ok(())
}
