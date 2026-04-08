//! PairingGuard — stub implementation.
//!
//! Zeroclaw's full PairingGuard handles a pairing-code allowlist flow where
//! new users are admitted by exchanging a one-time code. OpenAgent uses the
//! `guard` module (GuardDb) for the same purpose.
//!
//! This stub satisfies the `PairingGuard` type used in `channels/telegram.rs`
//! while keeping the channel compilation clean. All calls fail-open:
//! if `allowed_users` is empty the guard is disabled and every sender passes.

/// Pairing guard — validates senders against an allowlist.
pub struct PairingGuard {
    enabled: bool,
    allowed: Vec<String>,
}

impl PairingGuard {
    /// Create a new guard. `enabled = false` → allow all senders.
    pub fn new(enabled: bool, allowed: &[String]) -> Self {
        Self {
            enabled,
            allowed: allowed.to_vec(),
        }
    }

    /// Returns `true` if the sender is allowed to use the bot.
    pub fn is_allowed(&self, sender: &str) -> bool {
        if !self.enabled || self.allowed.is_empty() {
            return true;
        }
        self.allowed.iter().any(|a| a == sender || a == "*")
    }

    /// Extract a pairing code from a message (stub — always returns None).
    pub fn pairing_code(_text: &str) -> Option<String> {
        None
    }

    /// Handle a pairing attempt (stub — always returns None).
    pub fn handle_pairing(&mut self, _sender: &str, _text: &str) -> Option<String> {
        None
    }
}
