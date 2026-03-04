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

func TestBuildServerStatusTool(t *testing.T) {
	rt := newTelegramRuntime(1, "hash", "token")
	server := buildServer(rt)
	resp, err := server.HandleRequest(context.Background(), mcplite.ToolCallRequest{
		ID:     "x",
		Type:   mcplite.TypeToolCall,
		Tool:   "telegram.status",
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
	if result.Result == nil {
		t.Fatal("expected result payload")
	}
	var payload map[string]any
	if err := json.Unmarshal([]byte(*result.Result), &payload); err != nil {
		t.Fatalf("result is not valid JSON: %v", err)
	}
	if payload["backend"] != "gotd.td" {
		t.Fatalf("unexpected backend value: %#v", payload["backend"])
	}
}

func TestInt64Param(t *testing.T) {
	value, err := int64Param(map[string]any{"user_id": float64(42)}, "user_id")
	if err != nil {
		t.Fatalf("int64Param failed: %v", err)
	}
	if value != 42 {
		t.Fatalf("unexpected value: %d", value)
	}
}

func TestTelegramRuntimeValidationAndEvents(t *testing.T) {
	rt := newTelegramRuntime(1, "hash", "token")
	rt.stop()
	status := rt.status()
	if status["backend"] != "gotd.td" {
		t.Fatalf("unexpected backend: %+v", status)
	}
	rt.setError("x")
	rt.emitConnectionStatus()
	event, ok := rt.pollEvent()
	if !ok {
		t.Fatal("expected queued event")
	}
	if event.Event != "telegram.connection.status" {
		t.Fatalf("unexpected event: %+v", event)
	}
	if _, err := rt.sendMessage(context.Background(), 1, 2, ""); err == nil {
		t.Fatal("expected text validation error")
	}
}

func TestHandleConnPongAndEvent(t *testing.T) {
	rt := newTelegramRuntime(0, "", "")
	server := buildServer(rt)
	left, right := net.Pipe()
	defer left.Close()
	defer right.Close()

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go handleConn(ctx, left, server, rt)

	reader := bufio.NewReader(right)
	_ = right.SetReadDeadline(time.Now().Add(2 * time.Second))
	first, err := reader.ReadString('\n')
	if err != nil {
		t.Fatalf("read initial event failed: %v", err)
	}
	var evt map[string]any
	if err := json.Unmarshal([]byte(first), &evt); err != nil {
		t.Fatalf("event decode failed: %v", err)
	}
	if evt["type"] != "event" {
		t.Fatalf("expected event frame, got %+v", evt)
	}

	_, err = right.Write([]byte(`{"id":"9","type":"ping"}` + "\n"))
	if err != nil {
		t.Fatalf("write ping failed: %v", err)
	}
	line, err := reader.ReadString('\n')
	if err != nil {
		t.Fatalf("read pong failed: %v", err)
	}
	var pong map[string]any
	if err := json.Unmarshal([]byte(line), &pong); err != nil {
		t.Fatalf("pong decode failed: %v", err)
	}
	if pong["type"] != "pong" {
		t.Fatalf("expected pong, got %+v", pong)
	}
}

func TestTelegramHelpers(t *testing.T) {
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

	raw, err := marshalJSON(map[string]any{"ok": true})
	if err != nil || raw == "" {
		t.Fatalf("marshalJSON failed: %v raw=%q", err, raw)
	}

	if got := firstNonEmpty("", "a", "b"); got != "a" {
		t.Fatalf("unexpected firstNonEmpty result: %q", got)
	}

	if _, err := randomInt63(); err != nil {
		t.Fatalf("randomInt63 failed: %v", err)
	}

	if _, err := int64Param(map[string]any{"x": "bad"}, "x"); err == nil {
		t.Fatal("expected parse error for invalid string")
	}
	if _, err := int64Param(map[string]any{}, "missing"); err == nil {
		t.Fatal("expected missing key error")
	}

	t.Setenv("A_ENV_INT", "123")
	value, err := readIntEnv("A_ENV_INT")
	if err != nil || value != 123 {
		t.Fatalf("readIntEnv unexpected result value=%d err=%v", value, err)
	}
	t.Setenv("A_ENV_BAD", "xyz")
	if _, err := readIntEnv("A_ENV_BAD"); err == nil {
		t.Fatal("expected invalid integer error")
	}
	t.Setenv("A_ENV_MISSING", "")
	if _, err := readIntEnv("A_ENV_MISSING"); err == nil {
		t.Fatal("expected missing env error")
	}
}
