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
			log.Printf("accept failed: %v", acceptErr)
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
			log.Printf("decode frame failed: %v", err)
			break
		}

		reqWG.Add(1)
		go func(f mcplite.Frame) {
			defer reqWG.Done()

			response, handleErr := server.HandleRequest(ctx, f)
			if handleErr != nil {
				id := frameID(f)
				if id == "" {
					log.Printf("unsupported non-request frame: %T", f)
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
				log.Printf("write frame failed: %v", err)
			}
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
