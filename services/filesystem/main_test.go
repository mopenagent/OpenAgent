package main

import (
	"bufio"
	"context"
	"encoding/json"
	"net"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/kmaneesh/openagent/services/sdk-go/mcplite"
)

func TestResolvePathWithinRoot(t *testing.T) {
	rt := &filesystemRuntime{root: t.TempDir()}
	_, rel, err := rt.resolvePath("a/b.txt", true)
	if err != nil {
		t.Fatalf("resolvePath error: %v", err)
	}
	if rel != "a/b.txt" {
		t.Fatalf("unexpected rel: %q", rel)
	}
}

func TestResolvePathRejectsEscape(t *testing.T) {
	rt := &filesystemRuntime{root: t.TempDir()}
	_, _, err := rt.resolvePath("../../etc/passwd", true)
	if err == nil {
		t.Fatal("expected path escape error")
	}
}

func TestWriteReadAppendEditFlow(t *testing.T) {
	root := t.TempDir()
	rt := &filesystemRuntime{root: root}

	if _, err := rt.writeFile(map[string]any{"path": "x/test.txt", "content": "hello"}); err != nil {
		t.Fatalf("writeFile error: %v", err)
	}
	if _, err := rt.appendFile(map[string]any{"path": "x/test.txt", "content": " world"}); err != nil {
		t.Fatalf("appendFile error: %v", err)
	}
	if _, err := rt.editFile(map[string]any{"path": "x/test.txt", "old_text": "world", "new_text": "go"}); err != nil {
		t.Fatalf("editFile error: %v", err)
	}
	payload, err := rt.readFile(map[string]any{"path": "x/test.txt"})
	if err != nil {
		t.Fatalf("readFile error: %v", err)
	}
	var decoded map[string]any
	if err := json.Unmarshal([]byte(payload), &decoded); err != nil {
		t.Fatalf("unmarshal error: %v", err)
	}
	if decoded["content"].(string) != "hello go" {
		t.Fatalf("unexpected content: %q", decoded["content"].(string))
	}
}

func TestSearchFilesystem(t *testing.T) {
	root := t.TempDir()
	mustWriteFile(t, filepath.Join(root, "cmd", "main.go"), "package main")
	mustWriteFile(t, filepath.Join(root, "README.md"), "docs")
	mustWriteFile(t, filepath.Join(root, ".hidden.txt"), "secret")

	res, err := searchFilesystem(searchOptions{Root: root, Query: "main", MaxResults: 10})
	if err != nil {
		t.Fatalf("searchFilesystem error: %v", err)
	}
	if len(res.Hits) == 0 {
		t.Fatalf("expected at least one hit")
	}
	if res.Hits[0].Path != "cmd/main.go" {
		t.Fatalf("unexpected first hit: %+v", res.Hits[0])
	}
}

func TestBuildServerToolCall(t *testing.T) {
	root := t.TempDir()
	mustWriteFile(t, filepath.Join(root, "src", "app.py"), "print('ok')")
	rt := &filesystemRuntime{root: root}
	server := buildServer(rt)

	resp, err := server.HandleRequest(context.Background(), mcplite.ToolCallRequest{ID: "1", Type: mcplite.TypeToolCall, Tool: "filesystem.list_dir", Params: map[string]any{"path": "src"}})
	if err != nil {
		t.Fatalf("HandleRequest error: %v", err)
	}
	result, ok := resp.(mcplite.ToolResultResponse)
	if !ok || result.Result == nil {
		t.Fatalf("unexpected response: %+v", resp)
	}
}

func TestHandleConnPingRoundTrip(t *testing.T) {
	rt := &filesystemRuntime{root: t.TempDir()}
	server := buildServer(rt)
	left, right := net.Pipe()
	defer left.Close()
	defer right.Close()

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go handleConn(ctx, left, server)

	_, err := right.Write([]byte(`{"id":"1","type":"ping"}` + "\n"))
	if err != nil {
		t.Fatalf("write ping failed: %v", err)
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

func mustWriteFile(t *testing.T, path, content string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatalf("mkdir failed: %v", err)
	}
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("write file failed: %v", err)
	}
}
