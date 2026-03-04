package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"sync"
	"syscall"
	"time"

	"github.com/kmaneesh/openagent/services/sdk-go/mcplite"
)

const defaultSocketPath = "data/sockets/whatsapp.sock"

func main() {
	if err := run(); err != nil {
		log.Fatalf("whatsapp service failed: %v", err)
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
		"service":     "whatsapp",
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
				"service": "whatsapp",
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
					"chat_id": map[string]any{"type": "string", "description": "Destination chat ID/JID."},
					"text":    map[string]any{"type": "string", "description": "Text message to send."},
				},
				"required": []string{"chat_id", "text"},
			},
		},
	}

	server := mcplite.NewServer(tools, "ready")
	server.RegisterToolHandler("whatsapp.status", func(_ context.Context, _ map[string]any) (string, error) {
		return compactJSON(map[string]any{
			"running":   true,
			"connected": false,
			"backend":   "service",
		})
	})
	server.RegisterToolHandler("whatsapp.link_state", func(_ context.Context, _ map[string]any) (string, error) {
		return compactJSON(map[string]any{
			"linked":      false,
			"session_age": nil,
			"self_id":     nil,
		})
	})
	server.RegisterToolHandler("whatsapp.send_text", func(_ context.Context, params map[string]any) (string, error) {
		chatID, _ := params["chat_id"].(string)
		text, _ := params["text"].(string)
		if chatID == "" {
			return "", errors.New("chat_id is required")
		}
		if text == "" {
			return "", errors.New("text is required")
		}
		return compactJSON(map[string]any{
			"ok":      true,
			"chat_id": chatID,
			"text":    text,
		})
	})
	return server
}

func handleConn(ctx context.Context, conn net.Conn, server *mcplite.Server) {
	defer conn.Close()

	decoder := mcplite.NewDecoder(conn)
	encoder := mcplite.NewEncoder(conn)
	var writeMu sync.Mutex
	var reqWG sync.WaitGroup

	emitEvent := func(event mcplite.EventFrame) {
		writeMu.Lock()
		defer writeMu.Unlock()
		if err := encoder.WriteFrame(event); err != nil {
			log.Printf("write event failed: %v", err)
		}
	}

	emitEvent(mcplite.EventFrame{
		Type:  mcplite.TypeEvent,
		Event: "whatsapp.connection.status",
		Data:  map[string]any{"connected": false, "backend": "service"},
	})
	emitEvent(mcplite.EventFrame{
		Type:  mcplite.TypeEvent,
		Event: "whatsapp.qr",
		Data:  map[string]any{"qr": ""},
	})

	for {
		frame, err := decoder.Next()
		if err != nil {
			if errors.Is(err, io.EOF) {
				break
			}
			mcplite.LogEvent("ERROR", "decode frame failed", map[string]any{
				"service": "whatsapp",
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
						"service": "whatsapp",
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
					"service":    "whatsapp",
					"request_id": requestID,
					"tool":       tool,
					"error":      err.Error(),
				})
				return
			}
			mcplite.LogEvent("INFO", "request handled", map[string]any{
				"service":     "whatsapp",
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

func compactJSON(v map[string]any) (string, error) {
	data, err := json.Marshal(v)
	if err != nil {
		return "", err
	}
	return string(data), nil
}
