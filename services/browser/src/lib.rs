//! browser_lib — public surface for integration and e2e tests.
//!
//! `main.rs` owns `metrics` and `tools` (internal).
//! Everything else is exposed here so tests in `tests/` can import directly.

pub mod cache;
pub mod extract;
pub mod fetch;
pub mod search;
