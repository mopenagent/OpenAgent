package main

import (
	"context"
	"testing"

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
