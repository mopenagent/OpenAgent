//! MQTT → SOP event fan-in listener.
//!
//! **Note:** This is NOT a [`zeroclaw::channels::Channel`] implementor.
//! It routes MQTT publishes to the SOP engine, not to the agent chat loop.
//! Kept as a module placeholder so the SOP subsystem can be wired here
//! when the SOP engine is integrated into openagent.
//!
//! Config block in `config/channels.toml` (or `config/openagent.toml`):
//! ```toml
//! [mqtt]
//! enabled    = true
//! broker_url = "mqtt://localhost:1883"
//! client_id  = "openagent"
//! topics     = ["openagent/events/#"]
//! ```
//!
//! ## Integration status
//! The zeroclaw `mqtt.rs` implementation depends on `crate::sop::*` (SOP engine,
//! audit logger) which are not yet ported to openagent. This stub exists to
//! reserve the module path and document the integration plan.
//!
//! To activate: wire `run_mqtt_sop_listener()` from zeroclaw once the SOP
//! engine (`openagent/src/sop/`) is fully operational.

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MqttConfig {
    #[serde(default)]
    pub enabled: bool,
    /// MQTT broker URL (e.g. `mqtt://localhost:1883`).
    #[serde(default)]
    pub broker_url: String,
    #[serde(default = "default_client_id")]
    pub client_id: String,
    /// Topics to subscribe (wildcards supported).
    #[serde(default)]
    pub topics: Vec<String>,
}

fn default_client_id() -> String {
    "openagent".to_string()
}
