package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/kmaneesh/openagent/services/sdk-go/mcplite"
)

const defaultSocketPath = "data/sockets/hello.sock"

func main() {
	if err := run(); err != nil {
		log.Fatalf("hello service failed: %v", err)
	}
}

func run() error {
	ctx, cancel := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer cancel()

	socketPath := os.Getenv("OPENAGENT_SOCKET_PATH")
	if socketPath == "" {
		socketPath = defaultSocketPath
	}

	if err := os.MkdirAll(filepath.Dir(socketPath), 0o755); err != nil {
		return fmt.Errorf("create socket directory: %w", err)
	}
	if err := os.Remove(socketPath); err != nil && !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("remove stale socket: %w", err)
	}

	listener, err := net.Listen("unix", socketPath)
	if err != nil {
		return fmt.Errorf("listen on socket %q: %w", socketPath, err)
	}
	defer func() {
		_ = listener.Close()
		_ = os.Remove(socketPath)
	}()
	mcplite.LogEvent("INFO", "service listening", map[string]any{
		"service":     "hello",
		"socket_path": socketPath,
	})

	server := buildServer()
	var connWG sync.WaitGroup

	go func() {
		<-ctx.Done()
		_ = listener.Close()
	}()

	for {
		conn, acceptErr := listener.Accept()
		if acceptErr != nil {
			if errors.Is(acceptErr, net.ErrClosed) || ctx.Err() != nil {
				break
			}
			mcplite.LogEvent("ERROR", "accept failed", map[string]any{
				"service": "hello",
				"error":   acceptErr.Error(),
			})
			continue
		}

		connWG.Add(1)
		go func(c net.Conn) {
			defer connWG.Done()
			handleConn(ctx, c, server)
		}(conn)
	}

	connWG.Wait()
	return nil
}

func buildServer() *mcplite.Server {
	tools := []mcplite.ToolDefinition{
		{
			Name:        "hello.reply",
			Description: "Reply with a funny message when the input is a hello-style greeting.",
			Params: map[string]any{
				"type": "object",
				"properties": map[string]any{
					"text": map[string]any{
						"type":        "string",
						"description": "Incoming message text, such as 'hello' or 'hi there'.",
					},
				},
				"required": []string{"text"},
			},
		},
	}

	server := mcplite.NewServer(tools, "ready")
	server.RegisterToolHandler("hello.reply", func(_ context.Context, params map[string]any) (string, error) {
		text, _ := params["text"].(string)
		return funnyReply(text), nil
	})
	return server
}

func handleConn(ctx context.Context, conn net.Conn, server *mcplite.Server) {
	defer conn.Close()

	decoder := mcplite.NewDecoder(conn)
	encoder := mcplite.NewEncoder(conn)
	var writeMu sync.Mutex
	var reqWG sync.WaitGroup

	for {
		frame, err := decoder.Next()
		if err != nil {
			if errors.Is(err, io.EOF) {
				break
			}
			mcplite.LogEvent("ERROR", "decode frame failed", map[string]any{
				"service": "hello",
				"error":   err.Error(),
			})
			break
		}

		reqWG.Add(1)
		go func(f mcplite.Frame) {
			defer reqWG.Done()
			start := time.Now()
			requestID := mcplite.RequestIDFromFrame(f)
			tool := mcplite.ToolNameFromFrame(f)
			outcome := "ok"

			response, handleErr := server.HandleRequest(ctx, f)
			if handleErr != nil {
				outcome = "error"
				id := frameID(f)
				if id == "" {
					mcplite.LogEvent("WARN", "unsupported frame", map[string]any{
						"service": "hello",
						"frame":   fmt.Sprintf("%T", f),
					})
					return
				}
				response = mcplite.ErrorResponse{
					ID:      id,
					Type:    mcplite.TypeError,
					Code:    "BAD_REQUEST",
					Message: handleErr.Error(),
				}
			}

			writeMu.Lock()
			defer writeMu.Unlock()
			if err := encoder.WriteFrame(response); err != nil {
				outcome = "error"
				mcplite.LogEvent("ERROR", "write frame failed", map[string]any{
					"service":    "hello",
					"request_id": requestID,
					"tool":       tool,
					"error":      err.Error(),
				})
				return
			}
			mcplite.LogEvent("INFO", "request handled", map[string]any{
				"service":     "hello",
				"request_id":  requestID,
				"tool":        tool,
				"outcome":     outcome,
				"duration_ms": float64(time.Since(start).Microseconds()) / 1000.0,
			})
		}(frame)
	}

	reqWG.Wait()
}

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

func funnyReply(text string) string {
	normalized := strings.TrimSpace(strings.ToLower(text))
	if strings.Contains(normalized, "hello") || strings.HasPrefix(normalized, "hi") || strings.Contains(normalized, "hey") {
		return "Hello there. I drank one byte of coffee and now I compile feelings."
	}
	return "I only tell my best joke after a hello. Try saying hello first."
}
