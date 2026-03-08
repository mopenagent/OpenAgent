package main

import "go.mau.fi/whatsmeow/proto/waE2E"

// extractText returns the plain text from a WhatsApp message proto and a
// kind label identifying the source (for logging/diagnostics).
// Returns ("", "unknown") when no extractable text is found.
func extractText(msg *waE2E.Message) (text, kind string) {
	if msg == nil {
		return "", "nil"
	}
	// Plain text DM
	if t := msg.GetConversation(); t != "" {
		return t, "conversation"
	}
	// Extended text: replies, link previews, forwarded messages
	if ext := msg.GetExtendedTextMessage(); ext != nil {
		if t := ext.GetText(); t != "" {
			return t, "extended_text"
		}
	}
	// Image (with optional caption)
	if img := msg.GetImageMessage(); img != nil {
		if cap := img.GetCaption(); cap != "" {
			return "[Image] " + cap, "image_caption"
		}
		return "[Image]", "image"
	}
	// Video (with optional caption)
	if vid := msg.GetVideoMessage(); vid != nil {
		if cap := vid.GetCaption(); cap != "" {
			return "[Video] " + cap, "video_caption"
		}
		return "[Video]", "video"
	}
	// Document (with optional caption)
	if doc := msg.GetDocumentMessage(); doc != nil {
		if cap := doc.GetCaption(); cap != "" {
			return "[Document] " + cap, "document_caption"
		}
		return "[Document]", "document"
	}
	// Audio / voice note
	if msg.GetAudioMessage() != nil {
		return "[Audio]", "audio"
	}
	// Sticker
	if msg.GetStickerMessage() != nil {
		return "[Sticker]", "sticker"
	}
	// Interactive button / list responses
	if btn := msg.GetButtonsResponseMessage(); btn != nil {
		if t := btn.GetSelectedDisplayText(); t != "" {
			return t, "buttons_response"
		}
	}
	if lst := msg.GetListResponseMessage(); lst != nil {
		if t := lst.GetTitle(); t != "" {
			return t, "list_response"
		}
	}
	return "", "unknown"
}
