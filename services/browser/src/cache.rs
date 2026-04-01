//! Simple in-memory TTL cache.
//!
//! Shared via `Arc<Mutex<Cache>>`. Two instances are used:
//!   - `search_cache` — query → JSON results (5 min TTL)
//!   - `fetch_cache`  — URL   → extracted text (1 hr TTL)

use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct Cache {
    entries: HashMap<String, (Instant, String)>,
    ttl: Duration,
}

impl Cache {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            entries: HashMap::new(),
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    /// Return cached value if still within TTL, else `None`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .get(key)
            .filter(|(ts, _)| ts.elapsed() < self.ttl)
            .map(|(_, v)| v.as_str())
    }

    pub fn set(&mut self, key: String, value: String) {
        self.entries.insert(key, (Instant::now(), value));
    }

    /// Test helper: create a cache with a sub-second TTL (milliseconds).
    #[cfg(test)]
    pub fn new_with_ttl_ms(ttl_ms: u64) -> Self {
        Self {
            entries: std::collections::HashMap::new(),
            ttl: std::time::Duration::from_millis(ttl_ms),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn miss_on_empty_cache() {
        let c = Cache::new(60);
        assert!(c.get("key").is_none());
    }

    #[test]
    fn hit_after_set() {
        let mut c = Cache::new(60);
        c.set("k".into(), "v".into());
        assert_eq!(c.get("k"), Some("v"));
    }

    #[test]
    fn different_keys_are_independent() {
        let mut c = Cache::new(60);
        c.set("a".into(), "1".into());
        c.set("b".into(), "2".into());
        assert_eq!(c.get("a"), Some("1"));
        assert_eq!(c.get("b"), Some("2"));
        assert!(c.get("c").is_none());
    }

    #[test]
    fn overwrite_replaces_value() {
        let mut c = Cache::new(60);
        c.set("k".into(), "old".into());
        c.set("k".into(), "new".into());
        assert_eq!(c.get("k"), Some("new"));
    }

    #[test]
    fn entry_expires_after_ttl() {
        let mut c = Cache::new_with_ttl_ms(50);
        c.set("k".into(), "v".into());
        assert_eq!(c.get("k"), Some("v"));
        sleep(Duration::from_millis(100));
        assert!(c.get("k").is_none(), "entry should have expired");
    }
}
