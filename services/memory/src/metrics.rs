//! Daily-rotating JSONL metrics writer — one line per memory operation.
//!
//! Output: logs/memory-metrics-YYYY-MM-DD.jsonl
//! Format: {"ts_ms":…,"service":"memory","op":"store|search","status":"ok|error",…}

use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Appends one JSON line per memory operation to a daily-rotating JSONL file.
#[derive(Debug)]
pub struct MetricsWriter {
    inner: Arc<Mutex<MetricsInner>>,
    logs_dir: PathBuf,
}

#[derive(Debug)]
struct MetricsInner {
    file: File,
    current_date: String,
}

impl MetricsWriter {
    pub fn new(logs_dir: &str) -> anyhow::Result<Self> {
        let dir = PathBuf::from(logs_dir);
        fs::create_dir_all(&dir)?;
        let today = today_date();
        let file = open_metrics_file(&dir, &today)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(MetricsInner { file, current_date: today })),
            logs_dir: dir,
        })
    }

    pub fn record(&self, record: &Value) {
        let mut guard = self.inner.lock().expect("metrics mutex poisoned");
        let today = today_date();
        if guard.current_date != today {
            match open_metrics_file(&self.logs_dir, &today) {
                Ok(f) => {
                    guard.file = f;
                    guard.current_date = today;
                }
                Err(e) => {
                    eprintln!("metrics rotate error: {e}");
                    return;
                }
            }
        }
        if let Ok(line) = serde_json::to_string(record) {
            let _ = writeln!(guard.file, "{line}");
            let _ = guard.file.flush();
        }
    }
}

fn open_metrics_file(dir: &PathBuf, date: &str) -> anyhow::Result<File> {
    let path = dir.join(format!("memory-metrics-{date}.jsonl"));
    Ok(OpenOptions::new().create(true).append(true).open(path)?)
}

pub fn ts_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn today_date() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let (y, m, d) = days_to_ymd(secs / 86400);
    format!("{y:04}-{m:02}-{d:02}")
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let y = 1970 + days / 365;
    let rem = days % 365;
    (y, (1 + rem / 30).min(12), (1 + rem % 30).min(28))
}
