//! Typed error and `Result` alias for the sdk-rust MCP-lite library.
//!
//! All public library functions return [`Result<T>`]. Consumers (service
//! binaries) may continue to use `anyhow` in their own code — `Error`
//! implements `std::error::Error + Send + Sync`, so `?` propagation into
//! `anyhow::Result` works without any changes in the calling service.
//!
//! # Examples
//!
//! ```ignore
//! use sdk_rust::{Error, Result};
//!
//! fn my_fn() -> Result<()> {
//!     // io::Error and serde_json::Error convert automatically via `?`
//!     Ok(())
//! }
//! ```

use std::fmt;

/// Errors returned by the sdk-rust MCP-lite library.
///
/// Handler closures registered with [`crate::McpLiteServer::register_tool`]
/// are application code and may use `anyhow::Result<String>` freely — the
/// server converts any handler error to a protocol-level error frame, so
/// handler errors never surface as `Error` variants here.
#[derive(Debug)]
pub enum Error {
    /// I/O failure on the Unix socket — bind, accept, read, write, or flush.
    Io(std::io::Error),
    /// Frame serialization or deserialization failed.
    Codec(serde_json::Error),
    /// A frame type was received that this server does not handle.
    UnsupportedFrame,
    /// OTEL tracing initialisation failed.
    ///
    /// Carries the underlying error as a string to avoid leaking the
    /// `anyhow` type into the public API.
    OtelSetup(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "socket I/O error: {e}"),
            Self::Codec(e) => write!(f, "frame codec error: {e}"),
            Self::UnsupportedFrame => write!(f, "unsupported frame type"),
            Self::OtelSetup(msg) => write!(f, "OTEL setup failed: {msg}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Codec(e) => Some(e),
            Self::UnsupportedFrame | Self::OtelSetup(_) => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Codec(e)
    }
}

/// Result alias for sdk-rust library functions.
pub type Result<T> = std::result::Result<T, Error>;
