use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Matches `service.json` binary map keys like `"darwin/arm64"`.
pub type BinaryMap = HashMap<String, String>;

/// `health` block from `service.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct HealthConfig {
    pub interval_ms: u64,
    pub timeout_ms: u64,
    pub restart_backoff_ms: Vec<u64>,
    /// How long to wait for the socket file to appear after spawning (ms).
    /// Defaults to 30 000 ms — memory service needs ~10 s on first run to load ONNX model.
    #[serde(default = "default_startup_timeout_ms")]
    pub startup_timeout_ms: u64,
}

fn default_startup_timeout_ms() -> u64 {
    30_000
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            interval_ms: 5000,
            timeout_ms: 1000,
            restart_backoff_ms: vec![1000, 2000, 5000, 10000, 30000],
            startup_timeout_ms: default_startup_timeout_ms(),
        }
    }
}

/// A parsed `service.json` manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct ServiceManifest {
    pub name: String,
    pub description: Option<String>,
    pub version: Option<String>,
    pub binary: BinaryMap,
    pub socket: String,
    /// Set to false to prevent the service from being started at all.
    /// Useful for optional services whose binary is not yet built (e.g. tts).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional env vars to set on the child process (from `service.json` `env` block).
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub health: HealthConfig,
    /// Absolute path to the directory containing `service.json`. Filled in by `discover()`.
    #[serde(skip)]
    pub root: PathBuf,
}

fn default_true() -> bool {
    true
}

impl ServiceManifest {
    /// Resolve the binary path for `platform_key` (e.g. `"darwin/arm64"`) relative to `root`.
    pub fn binary_path(&self, platform_key: &str) -> Option<PathBuf> {
        let rel = self.binary.get(platform_key)?;
        // Binary paths in service.json are relative to the project root (not the service dir).
        Some(self.root.join(rel))
    }

    /// Resolve the socket path relative to `root`.
    pub fn socket_path(&self) -> PathBuf {
        self.root.join(&self.socket)
    }
}

/// Discover all `service.json` files under `services_dir` and parse them.
///
/// `project_root` is the repository root — used to resolve binary and socket paths.
pub fn discover(services_dir: &Path, project_root: &Path) -> Result<Vec<ServiceManifest>> {
    let pattern = services_dir
        .join("*/service.json")
        .to_string_lossy()
        .to_string();

    let mut manifests = Vec::new();

    for entry in glob::glob(&pattern).context("glob service.json files")? {
        let path = entry.context("glob entry")?;
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        let mut manifest: ServiceManifest = serde_json::from_str(&content)
            .with_context(|| format!("parse {}", path.display()))?;
        manifest.root = project_root.to_path_buf();
        manifests.push(manifest);
    }

    Ok(manifests)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_manifest(dir: &Path, name: &str, json: &str) {
        std::fs::create_dir_all(dir.join(name)).unwrap();
        let mut f = std::fs::File::create(dir.join(name).join("service.json")).unwrap();
        f.write_all(json.as_bytes()).unwrap();
    }

    #[test]
    fn discover_parses_manifest() {
        let tmp = TempDir::new().unwrap();
        write_manifest(
            tmp.path(),
            "guard",
            r#"{
                "name": "guard",
                "binary": {"darwin/arm64": "bin/guard-darwin-arm64"},
                "socket": "data/sockets/guard.sock",
                "health": {"interval_ms": 5000, "timeout_ms": 1000, "restart_backoff_ms": [1000]}
            }"#,
        );

        let root = PathBuf::from("/project");
        let manifests = discover(tmp.path(), &root).unwrap();
        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].name, "guard");
    }

    #[test]
    fn binary_path_resolves_relative_to_root() {
        let manifest = ServiceManifest {
            name: "guard".into(),
            description: None,
            version: None,
            binary: HashMap::from([("darwin/arm64".to_string(), "bin/guard-darwin-arm64".to_string())]),
            socket: "data/sockets/guard.sock".to_string(),
            env: HashMap::new(),
            health: HealthConfig::default(),
            root: PathBuf::from("/project"),
        };
        assert_eq!(
            manifest.binary_path("darwin/arm64").unwrap(),
            PathBuf::from("/project/bin/guard-darwin-arm64")
        );
    }

    #[test]
    fn binary_path_returns_none_for_missing_platform() {
        let manifest = ServiceManifest {
            name: "guard".into(),
            description: None,
            version: None,
            binary: HashMap::new(),
            socket: "data/sockets/guard.sock".to_string(),
            env: HashMap::new(),
            health: HealthConfig::default(),
            root: PathBuf::from("/project"),
        };
        assert!(manifest.binary_path("windows/amd64").is_none());
    }

    #[test]
    fn socket_path_resolves_relative_to_root() {
        let manifest = ServiceManifest {
            name: "guard".into(),
            description: None,
            version: None,
            binary: HashMap::new(),
            socket: "data/sockets/guard.sock".to_string(),
            env: HashMap::new(),
            health: HealthConfig::default(),
            root: PathBuf::from("/project"),
        };
        assert_eq!(
            manifest.socket_path(),
            PathBuf::from("/project/data/sockets/guard.sock")
        );
    }
}
