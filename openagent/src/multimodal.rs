//! Multimodal utilities — stubs for zeroclaw image marker counting.
//!
//! Full implementation would count `![image](...)` or base64 image markers
//! in a message list. Telegram uses this to decide whether to send a document
//! or an inline image.

use crate::providers::ChatMessage;

/// Count image markers in a list of messages.
/// Stub — returns 0 (OpenAgent handles media via the media_pipeline separately).
pub fn count_image_markers(messages: &[ChatMessage]) -> usize {
    messages.iter().filter(|m| {
        m.content.contains("![") || m.content.contains("data:image/")
    }).count()
}
