use anyhow::Result;
use url::Url;

/// Represents a standardized routing address for the omnibus channel service.
/// Examples:
///   - discord://1234567890/0987654321
///   - slack://work_workspace/C123456?thread=123.456
///   - telegram://bot_name/chat_id
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelAddress {
    url: Url,
}

impl ChannelAddress {
    /// Parse a URN string into a ChannelAddress.
    pub fn parse(address: &str) -> Result<Self> {
        let url = Url::parse(address).map_err(|e| anyhow::anyhow!("Invalid channel address format: {e}"))?;
        Ok(Self { url })
    }

    /// The platform identifier (e.g., "discord", "slack", "telegram").
    pub fn platform(&self) -> &str {
        self.url.scheme()
    }

    /// The service instance or workspace identifier (e.g., "work_workspace" or guild_id).
    #[allow(dead_code)]
    pub fn instance(&self) -> Option<&str> {
        self.url.host_str()
    }

    /// The specific chat or channel destination ID.
    pub fn chat_id(&self) -> &str {
        self.url.path().trim_start_matches('/')
    }

    /// Optional thread identifier (if applicable).
    pub fn thread_id(&self) -> Option<String> {
        let pairs = self.url.query_pairs();
        for (k, v) in pairs {
            if k == "thread" {
                return Some(v.into_owned());
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slack_address() {
        let addr = ChannelAddress::parse("slack://work_workspace/C123456?thread=123.456").unwrap();
        assert_eq!(addr.platform(), "slack");
        assert_eq!(addr.instance(), Some("work_workspace"));
        assert_eq!(addr.chat_id(), "C123456");
        assert_eq!(addr.thread_id().as_deref(), Some("123.456"));
    }

    #[test]
    fn test_discord_address() {
        let addr = ChannelAddress::parse("discord://1234567890/0987654321").unwrap();
        assert_eq!(addr.platform(), "discord");
        assert_eq!(addr.instance(), Some("1234567890")); // Guild ID
        assert_eq!(addr.chat_id(), "0987654321"); // Channel ID
        assert_eq!(addr.thread_id(), None);
    }
}
