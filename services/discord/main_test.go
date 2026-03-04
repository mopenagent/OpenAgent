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
	rt := newDiscordRuntime("token")
	server := buildServer(rt)
	resp, err := server.HandleRequest(context.Background(), mcplite.ToolCallRequest{
		ID:     "x",
		Type:   mcplite.TypeToolCall,
		Tool:   "discord.status",
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
	if payload["backend"] != "discordgo" {
		t.Fatalf("unexpected backend value: %#v", payload["backend"])
	}
}

func TestSendMessageValidation(t *testing.T) {
	rt := newDiscordRuntime("token")
	if _, err := rt.sendMessage("", "hello"); err == nil {
		t.Fatal("expected channel_id validation error")
	}
	if _, err := rt.sendMessage("123", ""); err == nil {
		t.Fatal("expected text validation error")
	}
	if _, err := rt.sendMessage("123", "hello"); err == nil {
		t.Fatal("expected not-started error")
	}
}

func TestRuntimeStatusAndEvents(t *testing.T) {
	rt := newDiscordRuntime("token")
	rt.setError("boom")
	status := rt.status()
	if status["backend"] != "discordgo" {
		t.Fatalf("unexpected backend: %+v", status)
	}
	if status["last_error"] != "boom" {
		t.Fatalf("unexpected error field: %+v", status)
	}
	rt.connected.Store(true)
	rt.authorized.Store(true)
	rt.emitConnectionStatus()
	event, ok := rt.pollEvent()
	if !ok {
		t.Fatal("expected queued event")
	}
	if event.Event != "discord.connection.status" {
		t.Fatalf("unexpected event: %+v", event)
	}
}

func TestHandleConnPongAndEvent(t *testing.T) {
	rt := newDiscordRuntime("token")
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

func TestHelpers(t *testing.T) {
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
}
