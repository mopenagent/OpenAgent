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

func TestFunnyReplyForHello(t *testing.T) {
	got := funnyReply("hello buddy")
	if got == "" {
		t.Fatal("expected non-empty response")
	}
	if got == "I only tell my best joke after a hello. Try saying hello first." {
		t.Fatalf("expected hello joke response, got fallback: %q", got)
	}
}

func TestFunnyReplyFallback(t *testing.T) {
	got := funnyReply("what is the weather")
	want := "I only tell my best joke after a hello. Try saying hello first."
	if got != want {
		t.Fatalf("unexpected fallback response: got %q want %q", got, want)
	}
}

func TestServerHandlesToolCall(t *testing.T) {
	server := buildServer()
	resp, err := server.HandleRequest(context.Background(), mcplite.ToolCallRequest{
		ID:     "r1",
		Type:   mcplite.TypeToolCall,
		Tool:   "hello.reply",
		Params: map[string]any{"text": "hello"},
	})
	if err != nil {
		t.Fatalf("HandleRequest returned error: %v", err)
	}
	result, ok := resp.(mcplite.ToolResultResponse)
	if !ok {
		t.Fatalf("expected ToolResultResponse, got %T", resp)
	}
	if result.Result == nil || *result.Result == "" {
		t.Fatalf("expected result text, got %+v", result)
	}
	if result.Error != nil {
		t.Fatalf("unexpected tool error: %q", *result.Error)
	}
}

func TestHandleConnRoundTrip(t *testing.T) {
	server := buildServer()
	left, right := net.Pipe()
	defer left.Close()
	defer right.Close()

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go handleConn(ctx, left, server)

	_, err := right.Write([]byte(`{"id":"1","type":"ping"}` + "\n"))
	if err != nil {
		t.Fatalf("write request failed: %v", err)
	}

	_ = right.SetReadDeadline(time.Now().Add(2 * time.Second))
	line, err := bufio.NewReader(right).ReadString('\n')
	if err != nil {
		t.Fatalf("read response failed: %v", err)
	}

	var raw map[string]any
	if err := json.Unmarshal([]byte(line), &raw); err != nil {
		t.Fatalf("json decode failed: %v", err)
	}
	if raw["type"] != "pong" {
		t.Fatalf("expected pong response, got %+v", raw)
	}
}

func TestFrameID(t *testing.T) {
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
}
