//! WhatsApp Web session storage — rusqlite backend for wa-rs (zeroclaw).
//!
//! `RusqliteStore` implements all wa-rs storage traits:
//! `SignalStore`, `AppSyncStore`, `ProtocolStore`, `DeviceStoreTrait`.
//! It persists E2E encryption keys and session state to a local SQLite file.
//!
//! Used internally by [`super::whatsapp_web::WhatsAppWebConfig`].
//! The store is created automatically when `WhatsAppWebChannel` is built;
//! direct use is only needed for migration or inspection.
//!
//! **Status:** Requires the `whatsapp-web` feature in zeroclaw's Cargo.toml.
//! Stubs compile unconditionally — storage operations are no-ops until the
//! feature is enabled.

// RusqliteStore lives in zeroclaw behind the `whatsapp-web` feature flag.
// Re-export only when that feature is compiled in; otherwise this module
// acts as a documentation placeholder.
//
// To access the type directly: `zeroclaw::channels::whatsapp_storage::RusqliteStore`
// after enabling `features = ["whatsapp-web"]` in vendor/zeroclaw/Cargo.toml.
