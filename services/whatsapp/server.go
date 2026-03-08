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
	}

	server := mcplite.NewServer(tools, "ready")

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
