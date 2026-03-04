package mcplite

import (
	"context"
	"errors"
	"testing"
)

func TestServerToolsListAndPing(t *testing.T) {
	srv := NewServer([]ToolDefinition{{Name: "x", Description: "d", Params: map[string]any{}}}, "")

	toolsResp, err := srv.HandleRequest(context.Background(), ToolListRequest{ID: "1", Type: TypeToolsList})
	if err != nil {
		t.Fatalf("tools.list failed: %v", err)
	}
	tools, ok := toolsResp.(ToolListResponse)
	if !ok {
		t.Fatalf("expected ToolListResponse, got %T", toolsResp)
	}
	if len(tools.Tools) != 1 || tools.Tools[0].Name != "x" {
		t.Fatalf("unexpected tools response: %+v", tools)
	}

	pingResp, err := srv.HandleRequest(context.Background(), PingRequest{ID: "2", Type: TypePing})
	if err != nil {
		t.Fatalf("ping failed: %v", err)
	}
	pong, ok := pingResp.(PongResponse)
	if !ok {
		t.Fatalf("expected PongResponse, got %T", pingResp)
	}
	if pong.Status != "ready" {
		t.Fatalf("expected default status ready, got %q", pong.Status)
	}
}

func TestServerToolErrorsAndUnsupported(t *testing.T) {
	srv := NewServer(nil, "ok")
	srv.RegisterToolHandler("boom", func(_ context.Context, _ map[string]any) (string, error) {
		return "", errors.New("boom")
	})

	notFoundResp, err := srv.HandleRequest(context.Background(), ToolCallRequest{
		ID:   "1",
		Type: TypeToolCall,
		Tool: "missing",
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	notFound, ok := notFoundResp.(ErrorResponse)
	if !ok {
		t.Fatalf("expected ErrorResponse, got %T", notFoundResp)
	}
	if notFound.Code != "TOOL_NOT_FOUND" {
		t.Fatalf("unexpected code: %q", notFound.Code)
	}

	boomResp, err := srv.HandleRequest(context.Background(), ToolCallRequest{
		ID:   "2",
		Type: TypeToolCall,
		Tool: "boom",
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	boom, ok := boomResp.(ToolResultResponse)
	if !ok {
		t.Fatalf("expected ToolResultResponse, got %T", boomResp)
	}
	if boom.Error == nil || *boom.Error == "" {
		t.Fatalf("expected tool error payload, got %+v", boom)
	}

	_, err = srv.HandleRequest(context.Background(), EventFrame{Type: TypeEvent, Event: "x", Data: map[string]any{}})
	if err == nil {
		t.Fatal("expected unsupported frame error")
	}
}
