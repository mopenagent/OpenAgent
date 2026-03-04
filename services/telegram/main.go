package main

import (
	"context"
	"crypto/rand"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"strconv"
	"sync"
	"sync/atomic"
	"syscall"
	"time"

	"github.com/gotd/td/telegram"
	"github.com/gotd/td/tg"
	"github.com/kmaneesh/openagent/services/sdk-go/mcplite"
)

const defaultSocketPath = "data/sockets/telegram.sock"

type telegramRuntime struct {
	appID    int
	appHash  string
	botToken string

	client *telegram.Client

	started    atomic.Bool
	connected  atomic.Bool
	authorized atomic.Bool

	lastErrMu sync.Mutex
	lastErr   string

	events chan mcplite.EventFrame
}

func newTelegramRuntime(appID int, appHash, botToken string) *telegramRuntime {
	return &telegramRuntime{
		appID:    appID,
		appHash:  appHash,
		botToken: botToken,
		events:   make(chan mcplite.EventFrame, 128),
	}
}

func (r *telegramRuntime) start(ctx context.Context) error {
	if r.started.Load() {
		return nil
	}
	if r.appID == 0 || r.appHash == "" || r.botToken == "" {
		return errors.New("missing TELEGRAM_APP_ID, TELEGRAM_APP_HASH, or TELEGRAM_BOT_TOKEN")
	}

	client := telegram.NewClient(r.appID, r.appHash, telegram.Options{})
	r.client = client

	r.started.Store(true)
	go func() {
		err := client.Run(ctx, func(runCtx context.Context) error {
			status, err := client.Auth().Status(runCtx)
			if err != nil {
				r.setError(fmt.Sprintf("auth status failed: %v", err))
				r.connected.Store(false)
				r.authorized.Store(false)
				r.emitConnectionStatus()
				return err
			}
			if !status.Authorized {
				if _, err := client.Auth().Bot(runCtx, r.botToken); err != nil {
					r.setError(fmt.Sprintf("bot auth failed: %v", err))
					r.connected.Store(false)
					r.authorized.Store(false)
					r.emitConnectionStatus()
					return err
				}
			}

			r.connected.Store(true)
			r.authorized.Store(true)
			r.setError("")
			r.emitConnectionStatus()

			<-runCtx.Done()
			return runCtx.Err()
		})
		if err != nil && !errors.Is(err, context.Canceled) {
			r.setError(err.Error())
		}
		r.connected.Store(false)
		r.authorized.Store(false)
		r.emitConnectionStatus()
	}()
	return nil
}

func (r *telegramRuntime) stop() {
	r.started.Store(false)
	r.connected.Store(false)
	r.authorized.Store(false)
}

func (r *telegramRuntime) status() map[string]any {
	status := map[string]any{
		"running":    r.started.Load(),
		"connected":  r.connected.Load(),
		"authorized": r.authorized.Load(),
		"backend":    "gotd.td",
	}
	if errText := r.errorText(); errText != "" {
		status["last_error"] = errText
	}
	return status
}

func (r *telegramRuntime) linkState() map[string]any {
	return map[string]any{
		"authorized": r.authorized.Load(),
		"connected":  r.connected.Load(),
		"backend":    "gotd.td",
	}
}

func (r *telegramRuntime) sendMessage(ctx context.Context, userID, accessHash int64, text string) (string, error) {
	if !r.started.Load() || !r.connected.Load() {
		return "", errors.New("telegram runtime is not connected")
	}
	if text == "" {
		return "", errors.New("text is required")
	}

	randomID, err := randomInt63()
	if err != nil {
		return "", err
	}

	_, err = r.client.API().MessagesSendMessage(ctx, &tg.MessagesSendMessageRequest{
		Peer: &tg.InputPeerUser{
			UserID:     userID,
			AccessHash: accessHash,
		},
		Message:  text,
		RandomID: randomID,
	})
	if err != nil {
		return "", err
	}
	return marshalJSON(map[string]any{
		"ok":      true,
		"user_id": userID,
	})
}

func (r *telegramRuntime) pollEvent() (mcplite.EventFrame, bool) {
	select {
	case evt := <-r.events:
		return evt, true
	default:
		return mcplite.EventFrame{}, false
	}
}

func (r *telegramRuntime) emitConnectionStatus() {
	data := map[string]any{
		"connected":  r.connected.Load(),
		"authorized": r.authorized.Load(),
		"backend":    "gotd.td",
	}
	if errText := r.errorText(); errText != "" {
		data["last_error"] = errText
	}
	select {
	case r.events <- mcplite.EventFrame{
		Type:  mcplite.TypeEvent,
		Event: "telegram.connection.status",
		Data:  data,
	}:
	default:
	}
}

func (r *telegramRuntime) setError(msg string) {
	r.lastErrMu.Lock()
	defer r.lastErrMu.Unlock()
	r.lastErr = msg
}

func (r *telegramRuntime) errorText() string {
	r.lastErrMu.Lock()
	defer r.lastErrMu.Unlock()
	return r.lastErr
}

func main() {
	if err := run(); err != nil {
		log.Fatalf("telegram service failed: %v", err)
	}
}

func run() error {
	ctx, cancel := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer cancel()

	socketPath := os.Getenv("OPENAGENT_SOCKET_PATH")
	if socketPath == "" {
		socketPath = defaultSocketPath
	}

	appID, err := readIntEnv("TELEGRAM_APP_ID", "OPENAGENT_TELEGRAM_APP_ID")
	if err != nil {
		return err
	}
	appHash := firstNonEmpty(os.Getenv("TELEGRAM_APP_HASH"), os.Getenv("OPENAGENT_TELEGRAM_APP_HASH"))
	botToken := firstNonEmpty(os.Getenv("TELEGRAM_BOT_TOKEN"), os.Getenv("OPENAGENT_TELEGRAM_BOT_TOKEN"))

	runtime := newTelegramRuntime(appID, appHash, botToken)
	if err := runtime.start(ctx); err != nil {
		return err
	}
	defer runtime.stop()

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
		"service":     "telegram",
		"socket_path": socketPath,
	})

	server := buildServer(runtime)
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
				"service": "telegram",
				"error":   acceptErr.Error(),
			})
			continue
		}
		connWG.Add(1)
		go func(c net.Conn) {
			defer connWG.Done()
			handleConn(ctx, c, server, runtime)
		}(conn)
	}

	connWG.Wait()
	return nil
}

func buildServer(rt *telegramRuntime) *mcplite.Server {
	tools := []mcplite.ToolDefinition{
		{
			Name:        "telegram.status",
			Description: "Return current Telegram service status.",
			Params:      map[string]any{"type": "object", "properties": map[string]any{}},
		},
		{
			Name:        "telegram.link_state",
			Description: "Return Telegram bot authorization and connection state.",
			Params:      map[string]any{"type": "object", "properties": map[string]any{}},
		},
		{
			Name:        "telegram.send_message",
			Description: "Send Telegram message with known peer identifiers (low-latency MTProto path).",
			Params: map[string]any{
				"type": "object",
				"properties": map[string]any{
					"user_id": map[string]any{
						"type":        "integer",
						"description": "Telegram user ID.",
					},
					"access_hash": map[string]any{
						"type":        "integer",
						"description": "Telegram access hash for the user peer.",
					},
					"text": map[string]any{
						"type":        "string",
						"description": "Text message content.",
					},
				},
				"required": []string{"user_id", "access_hash", "text"},
			},
		},
	}

	server := mcplite.NewServer(tools, "ready")
	server.RegisterToolHandler("telegram.status", func(_ context.Context, _ map[string]any) (string, error) {
		return marshalJSON(rt.status())
	})
	server.RegisterToolHandler("telegram.link_state", func(_ context.Context, _ map[string]any) (string, error) {
		return marshalJSON(rt.linkState())
	})
	server.RegisterToolHandler("telegram.send_message", func(ctx context.Context, params map[string]any) (string, error) {
		userID, err := int64Param(params, "user_id")
		if err != nil {
			return "", err
		}
		accessHash, err := int64Param(params, "access_hash")
		if err != nil {
			return "", err
		}
		text, _ := params["text"].(string)
		return rt.sendMessage(ctx, userID, accessHash, text)
	})
	return server
}

func handleConn(ctx context.Context, conn net.Conn, server *mcplite.Server, rt *telegramRuntime) {
	defer conn.Close()

	decoder := mcplite.NewDecoder(conn)
	encoder := mcplite.NewEncoder(conn)
	var writeMu sync.Mutex
	var reqWG sync.WaitGroup

	writeEvent := func(event mcplite.EventFrame) {
		writeMu.Lock()
		defer writeMu.Unlock()
		if err := encoder.WriteFrame(event); err != nil {
			log.Printf("write event failed: %v", err)
		}
	}

	// Emit startup status immediately for low-latency subscribers.
	writeEvent(mcplite.EventFrame{
		Type:  mcplite.TypeEvent,
		Event: "telegram.connection.status",
		Data:  rt.status(),
	})

	// Event pump.
	done := make(chan struct{})
	go func() {
		ticker := time.NewTicker(50 * time.Millisecond)
		defer ticker.Stop()
		for {
			select {
			case <-done:
				return
			case <-ticker.C:
				for {
					event, ok := rt.pollEvent()
					if !ok {
						break
					}
					writeEvent(event)
				}
			}
		}
	}()

	for {
		frame, err := decoder.Next()
		if err != nil {
			if errors.Is(err, io.EOF) {
				break
			}
			mcplite.LogEvent("ERROR", "decode frame failed", map[string]any{
				"service": "telegram",
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
						"service": "telegram",
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
					"service":    "telegram",
					"request_id": requestID,
					"tool":       tool,
					"error":      err.Error(),
				})
				return
			}
			mcplite.LogEvent("INFO", "request handled", map[string]any{
				"service":     "telegram",
				"request_id":  requestID,
				"tool":        tool,
				"outcome":     outcome,
				"duration_ms": float64(time.Since(start).Microseconds()) / 1000.0,
			})
		}(frame)
	}

	close(done)
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

func marshalJSON(v map[string]any) (string, error) {
	data, err := json.Marshal(v)
	if err != nil {
		return "", err
	}
	return string(data), nil
}

func int64Param(params map[string]any, key string) (int64, error) {
	value, ok := params[key]
	if !ok {
		return 0, fmt.Errorf("%s is required", key)
	}
	switch v := value.(type) {
	case int64:
		return v, nil
	case int:
		return int64(v), nil
	case float64:
		return int64(v), nil
	case json.Number:
		n, err := v.Int64()
		if err != nil {
			return 0, fmt.Errorf("invalid %s: %w", key, err)
		}
		return n, nil
	case string:
		n, err := strconv.ParseInt(v, 10, 64)
		if err != nil {
			return 0, fmt.Errorf("invalid %s: %w", key, err)
		}
		return n, nil
	default:
		return 0, fmt.Errorf("invalid %s type", key)
	}
}

func randomInt63() (int64, error) {
	var b [8]byte
	if _, err := rand.Read(b[:]); err != nil {
		return 0, err
	}
	value := int64(binary.BigEndian.Uint64(b[:]) & 0x7fffffffffffffff)
	if value == 0 {
		value = 1
	}
	return value, nil
}

func firstNonEmpty(values ...string) string {
	for _, v := range values {
		if v != "" {
			return v
		}
	}
	return ""
}

func readIntEnv(keys ...string) (int, error) {
	for _, key := range keys {
		raw := os.Getenv(key)
		if raw == "" {
			continue
		}
		value, err := strconv.Atoi(raw)
		if err != nil {
			return 0, fmt.Errorf("%s must be an integer: %w", key, err)
		}
		return value, nil
	}
	return 0, fmt.Errorf("missing required env var: %s", keys[0])
}
