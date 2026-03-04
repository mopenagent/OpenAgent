package main

import (
	"bufio"
	"context"
	"encoding/json"
	"net"
	"testing"
	"time"

	"github.com/kmaneesh/openagent/services/sdk-go/mcplite"
)

func TestStatusTool(t *testing.T) {
	server := buildServer()
	resp, err := server.HandleRequest(context.Background(), mcplite.ToolCallRequest{
		ID:     "1",
		Type:   mcplite.TypeToolCall,
		Tool:   "whatsapp.status",
		Params: map[string]any{},
	})
	if err != nil {
		t.Fatalf("HandleRequest returned error: %v", err)
	}
	result, ok := resp.(mcplite.ToolResultResponse)
	if !ok {
		t.Fatalf("expected ToolResultResponse, got %T", resp)
	}
	if result.Error != nil {
		t.Fatalf("unexpected tool error: %s", *result.Error)
	}
	if result.Result == nil || *result.Result == "" {
		t.Fatal("expected non-empty result payload")
	}
}

func TestSendTextValidation(t *testing.T) {
	server := buildServer()
	resp, err := server.HandleRequest(context.Background(), mcplite.ToolCallRequest{
		ID:     "2",
		Type:   mcplite.TypeToolCall,
		Tool:   "whatsapp.send_text",
		Params: map[string]any{"chat_id": "x@s.whatsapp.net", "text": "hi"},
	})
	if err != nil {
		t.Fatalf("HandleRequest returned error: %v", err)
	}
	result, ok := resp.(mcplite.ToolResultResponse)
	if !ok {
		t.Fatalf("expected ToolResultResponse, got %T", resp)
	}
	if result.Error != nil {
		t.Fatalf("unexpected tool error: %s", *result.Error)
	}
	if result.Result == nil {
		t.Fatal("expected non-nil result")
	}
}

func TestHandleConnEmitsEventsAndPong(t *testing.T) {
	server := buildServer()
	left, right := net.Pipe()
	defer left.Close()
	defer right.Close()

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go handleConn(ctx, left, server)

	reader := bufio.NewReader(right)
	_ = right.SetReadDeadline(time.Now().Add(2 * time.Second))
	first, err := reader.ReadString('\n')
	if err != nil {
		t.Fatalf("read first event failed: %v", err)
	}
	second, err := reader.ReadString('\n')
	if err != nil {
		t.Fatalf("read second event failed: %v", err)
	}

	var evt map[string]any
	if err := json.Unmarshal([]byte(first), &evt); err != nil {
		t.Fatalf("decode first event failed: %v", err)
	}
	if evt["type"] != "event" {
		t.Fatalf("expected event frame, got %+v", evt)
	}
	if err := json.Unmarshal([]byte(second), &evt); err != nil {
		t.Fatalf("decode second event failed: %v", err)
	}
	if evt["type"] != "event" {
		t.Fatalf("expected event frame, got %+v", evt)
	}

	_, err = right.Write([]byte(`{"id":"9","type":"ping"}` + "\n"))
	if err != nil {
		t.Fatalf("write ping failed: %v", err)
	}
	pongLine, err := reader.ReadString('\n')
	if err != nil {
		t.Fatalf("read pong failed: %v", err)
	}
	var pong map[string]any
	if err := json.Unmarshal([]byte(pongLine), &pong); err != nil {
		t.Fatalf("decode pong failed: %v", err)
	}
	if pong["type"] != "pong" {
		t.Fatalf("expected pong, got %+v", pong)
	}
}

func TestFrameIDAndCompactJSON(t *testing.T) {
	if got := frameID(mcplite.ToolListRequest{ID: "1", Type: mcplite.TypeToolsList}); got != "1" {
		t.Fatalf("unexpected tool list id: %q", got)
	}
	if got := frameID(mcplite.ToolCallRequest{ID: "2", Type: mcplite.TypeToolCall}); got != "2" {
		t.Fatalf("unexpected tool call id: %q", got)
	}
	if got := frameID(mcplite.PingRequest{ID: "3", Type: mcplite.TypePing}); got != "3" {
		t.Fatalf("unexpected ping id: %q", got)
	}
	if got := frameID(mcplite.EventFrame{Type: mcplite.TypeEvent, Event: "x", Data: map[string]any{}}); got != "" {
		t.Fatalf("expected empty id, got %q", got)
	}
	raw, err := compactJSON(map[string]any{"ok": true})
	if err != nil {
		t.Fatalf("compactJSON failed: %v", err)
	}
	if raw == "" {
		t.Fatal("expected non-empty json")
	}
}
