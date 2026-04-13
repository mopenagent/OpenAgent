package main

import (
	"context"
	"encoding/json"

	"github.com/kmaneesh/openagent/services/sdk-go/mcplite"
)

// buildServer wires tool definitions and handlers for the MCP-lite server.
func buildServer(rt *waRuntime) *mcplite.Server {
	tools := []mcplite.ToolDefinition{
		{
			Name:        "whatsapp.status",
			Description: "Return the current WhatsApp service status.",
			Params:      map[string]any{"type": "object", "properties": map[string]any{}},
		},
		{
			Name:        "whatsapp.link_state",
			Description: "Return the current link state and session metadata.",
			Params:      map[string]any{"type": "object", "properties": map[string]any{}},
		},
		{
			Name:        "whatsapp.send_text",
			Description: "Send a text message to a WhatsApp chat.",
			Params: map[string]any{
				"type": "object",
				"properties": map[string]any{
					"chat_id": map[string]any{
						"type":        "string",
						"description": "Destination chat ID/JID (e.g. 15551234567@s.whatsapp.net).",
					},
					"text": map[string]any{
						"type":        "string",
						"description": "Text message to send.",
					},
				},
				"required": []string{"chat_id", "text"},
			},
		},
		{
			Name:        "whatsapp.typing_start",
			Description: "Send a typing (composing) presence indicator to a WhatsApp chat. Clears automatically after ~10 s.",
			Params: map[string]any{
				"type": "object",
				"properties": map[string]any{
					"chat_id": map[string]any{
						"type":        "string",
						"description": "Destination chat ID/JID.",
					},
				},
				"required": []string{"chat_id"},
			},
		},
		{
			Name:        "whatsapp.qr",
			Description: "Return the current WhatsApp QR code (if awaiting pairing) and connection state.",
			Params:      map[string]any{"type": "object", "properties": map[string]any{}},
		},
		{
			Name:        "whatsapp.send_media",
			Description: "Upload and send a local file (image, video, audio, or document) to a WhatsApp chat.",
			Params: map[string]any{
				"type": "object",
				"properties": map[string]any{
					"chat_id": map[string]any{
						"type":        "string",
						"description": "Destination chat ID/JID.",
					},
					"file_path": map[string]any{
						"type":        "string",
						"description": "Absolute path to the file to send.",
					},
					"mime_type": map[string]any{
						"type":        "string",
						"description": "MIME type (e.g. image/jpeg). Auto-detected from extension if omitted.",
					},
					"caption": map[string]any{
						"type":        "string",
						"description": "Optional caption shown under the media.",
					},
				},
				"required": []string{"chat_id", "file_path"},
			},
		},
	}

	server := mcplite.NewServer(tools, "ready")

	server.RegisterToolHandler("whatsapp.qr", func(_ context.Context, _ map[string]any) (string, error) {
		qr := rt.latestQR()
		return marshalJSON(map[string]any{
			"qr_text":   qr,
			"connected": rt.connected.Load(),
		})
	})
	server.RegisterToolHandler("whatsapp.status", func(_ context.Context, _ map[string]any) (string, error) {
		return marshalJSON(rt.status())
	})
	server.RegisterToolHandler("whatsapp.link_state", func(_ context.Context, _ map[string]any) (string, error) {
		return marshalJSON(rt.linkState())
	})
	server.RegisterToolHandler("whatsapp.send_text", func(_ context.Context, params map[string]any) (string, error) {
		chatID, _ := params["chat_id"].(string)
		text, _ := params["text"].(string)
		return rt.sendText(chatID, text)
	})

	server.RegisterToolHandler("whatsapp.typing_start", func(_ context.Context, params map[string]any) (string, error) {
		chatID, _ := params["chat_id"].(string)
		return rt.sendTyping(chatID)
	})

	server.RegisterToolHandler("whatsapp.send_media", func(_ context.Context, params map[string]any) (string, error) {
		chatID, _ := params["chat_id"].(string)
		filePath, _ := params["file_path"].(string)
		mimeType, _ := params["mime_type"].(string)
		caption, _ := params["caption"].(string)
		return rt.sendMedia(chatID, filePath, mimeType, caption)
	})

	return server
}

// frameID extracts the correlation ID from any request frame.
func frameID(frame mcplite.Frame) string {
	switch v := frame.(type) {
	case mcplite.ToolListRequest:
		return v.ID
	case mcplite.ToolCallRequest:
		return v.ID
	case mcplite.PingRequest:
		return v.ID
	default:
		return ""
	}
}

func marshalJSON(v map[string]any) (string, error) {
	data, err := json.Marshal(v)
	if err != nil {
		return "", err
	}
	return string(data), nil
}
