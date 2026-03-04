package mcplite

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
)

func TestDecodeFrameToolCall(t *testing.T) {
	frame, err := DecodeFrame([]byte(`{"id":"1","type":"tool.call","tool":"echo","params":{"text":"hi"}}`))
	if err != nil {
		t.Fatalf("DecodeFrame failed: %v", err)
	}

	req, ok := frame.(ToolCallRequest)
	if !ok {
		t.Fatalf("expected ToolCallRequest, got %T", frame)
	}
	if req.ID != "1" || req.Tool != "echo" {
		t.Fatalf("unexpected request: %+v", req)
	}
}

func TestEncodeFrameAddsDelimiter(t *testing.T) {
	f := PingRequest{ID: "health-1", Type: TypePing}
	data, err := EncodeFrame(f)
	if err != nil {
		t.Fatalf("EncodeFrame failed: %v", err)
	}
	if !strings.HasSuffix(string(data), "\n") {
		t.Fatalf("expected newline delimiter, got %q", string(data))
	}
}

func TestEncodeFrameNormalizesToolCallParams(t *testing.T) {
	data, err := EncodeFrame(ToolCallRequest{
		ID:   "2",
		Type: TypeToolCall,
		Tool: "echo",
	})
	if err != nil {
		t.Fatalf("EncodeFrame failed: %v", err)
	}

	var raw map[string]any
	if err := json.Unmarshal([]byte(strings.TrimSpace(string(data))), &raw); err != nil {
		t.Fatalf("unmarshal encoded frame failed: %v", err)
	}

	params, ok := raw["params"].(map[string]any)
	if !ok {
		t.Fatalf("expected params object, got %#v", raw["params"])
	}
	if len(params) != 0 {
		t.Fatalf("expected empty params object, got %#v", params)
	}
}

func TestEncodeFrameNormalizesEventData(t *testing.T) {
	data, err := EncodeFrame(EventFrame{
		Type:  TypeEvent,
		Event: "message.received",
	})
	if err != nil {
		t.Fatalf("EncodeFrame failed: %v", err)
	}

	var raw map[string]any
	if err := json.Unmarshal([]byte(strings.TrimSpace(string(data))), &raw); err != nil {
		t.Fatalf("unmarshal encoded frame failed: %v", err)
	}

	eventData, ok := raw["data"].(map[string]any)
	if !ok {
		t.Fatalf("expected data object, got %#v", raw["data"])
	}
	if len(eventData) != 0 {
		t.Fatalf("expected empty data object, got %#v", eventData)
	}
}

func TestServerHandleRequest(t *testing.T) {
	srv := NewServer([]ToolDefinition{
		{
			Name:        "echo",
			Description: "Echoes text",
			Params:      map[string]any{"type": "object"},
		},
	}, "ready")
	srv.RegisterToolHandler("echo", func(_ context.Context, params map[string]any) (string, error) {
		text, _ := params["text"].(string)
		return text, nil
	})

	resp, err := srv.HandleRequest(context.Background(), ToolCallRequest{
		ID:     "9",
		Type:   TypeToolCall,
		Tool:   "echo",
		Params: map[string]any{"text": "hello"},
	})
	if err != nil {
		t.Fatalf("HandleRequest failed: %v", err)
	}

	result, ok := resp.(ToolResultResponse)
	if !ok {
		t.Fatalf("expected ToolResultResponse, got %T", resp)
	}
	if result.Result == nil || *result.Result != "hello" {
		t.Fatalf("unexpected result: %+v", result)
	}
	if result.Error != nil {
		t.Fatalf("unexpected error: %v", *result.Error)
	}
}
