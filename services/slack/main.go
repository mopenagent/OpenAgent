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
	"sync/atomic"
	"syscall"
	"time"

	"github.com/kmaneesh/openagent/services/sdk-go/mcplite"
	"github.com/slack-go/slack"
)

const defaultSocketPath = "data/sockets/slack.sock"

type slackRuntime struct {
	token string
	api   *slack.Client

	started    atomic.Bool
	connected  atomic.Bool
	authorized atomic.Bool

	botUserID string
	teamID    string

	lastErrMu sync.Mutex
	lastErr   string

	events chan mcplite.EventFrame
}

func newSlackRuntime(token string) *slackRuntime {
	return &slackRuntime{
		token:  token,
		events: make(chan mcplite.EventFrame, 128),
	}
}

func (r *slackRuntime) start(ctx context.Context) error {
	_ = ctx
	if r.started.Load() {
		return nil
	}
	if r.token == "" {
		return errors.New("missing SLACK_BOT_TOKEN or OPENAGENT_SLACK_BOT_TOKEN")
	}

	api := slack.New(r.token)
	auth, err := api.AuthTest()
	if err != nil {
		r.setError(err.Error())
		r.emitConnectionStatus()
		return err
	}

	r.api = api
	r.botUserID = auth.UserID
	r.teamID = auth.TeamID
	r.started.Store(true)
	r.connected.Store(true)
	r.authorized.Store(true)
	r.setError("")
	r.emitConnectionStatus()
	return nil
}

func (r *slackRuntime) stop() {
	r.started.Store(false)
	r.connected.Store(false)
	r.authorized.Store(false)
	r.emitConnectionStatus()
}

func (r *slackRuntime) status() map[string]any {
	status := map[string]any{
		"running":    r.started.Load(),
		"connected":  r.connected.Load(),
		"authorized": r.authorized.Load(),
		"backend":    "slack-go",
	}
	if r.botUserID != "" {
		status["bot_user_id"] = r.botUserID
	}
	if r.teamID != "" {
		status["team_id"] = r.teamID
	}
	if msg := r.errorText(); msg != "" {
		status["last_error"] = msg
	}
	return status
}

func (r *slackRuntime) linkState() map[string]any {
	return map[string]any{
		"authorized":  r.authorized.Load(),
		"connected":   r.connected.Load(),
		"backend":     "slack-go",
		"bot_user_id": r.botUserID,
		"team_id":     r.teamID,
	}
}

func (r *slackRuntime) sendMessage(channelID, text string) (string, error) {
	if channelID == "" {
		return "", errors.New("channel_id is required")
	}
	if text == "" {
		return "", errors.New("text is required")
	}
	if !r.started.Load() || r.api == nil {
		return "", errors.New("slack runtime is not started")
	}

	channel, timestamp, err := r.api.PostMessage(channelID, slack.MsgOptionText(text, false))
	if err != nil {
		r.setError(err.Error())
		r.emitConnectionStatus()
		return "", err
	}

	return marshalJSON(map[string]any{
		"ok":         true,
		"channel_id": channel,
		"ts":         timestamp,
	})
}

func (r *slackRuntime) pollEvent() (mcplite.EventFrame, bool) {
	select {
	case evt := <-r.events:
		return evt, true
	default:
		return mcplite.EventFrame{}, false
	}
}

func (r *slackRuntime) emitConnectionStatus() {
	data := map[string]any{
		"connected":  r.connected.Load(),
		"authorized": r.authorized.Load(),
		"backend":    "slack-go",
	}
	if msg := r.errorText(); msg != "" {
		data["last_error"] = msg
	}
	select {
	case r.events <- mcplite.EventFrame{
		Type:  mcplite.TypeEvent,
		Event: "slack.connection.status",
		Data:  data,
	}:
	default:
	}
}

func (r *slackRuntime) setError(msg string) {
	r.lastErrMu.Lock()
	defer r.lastErrMu.Unlock()
	r.lastErr = msg
}

func (r *slackRuntime) errorText() string {
	r.lastErrMu.Lock()
	defer r.lastErrMu.Unlock()
	return r.lastErr
}

func main() {
	if err := run(); err != nil {
		log.Fatalf("slack service failed: %v", err)
	}
}

func run() error {
	ctx, cancel := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer cancel()

	socketPath := os.Getenv("OPENAGENT_SOCKET_PATH")
	if socketPath == "" {
		socketPath = defaultSocketPath
	}
	token := firstNonEmpty(os.Getenv("SLACK_BOT_TOKEN"), os.Getenv("OPENAGENT_SLACK_BOT_TOKEN"))

	runtime := newSlackRuntime(token)
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
		"service":     "slack",
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
				"service": "slack",
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

func buildServer(rt *slackRuntime) *mcplite.Server {
	tools := []mcplite.ToolDefinition{
		{
			Name:        "slack.status",
			Description: "Return current Slack service status.",
			Params:      map[string]any{"type": "object", "properties": map[string]any{}},
		},
		{
			Name:        "slack.link_state",
			Description: "Return Slack auth/connect state.",
			Params:      map[string]any{"type": "object", "properties": map[string]any{}},
		},
		{
			Name:        "slack.send_message",
			Description: "Send a message to a Slack channel.",
			Params: map[string]any{
				"type": "object",
				"properties": map[string]any{
					"channel_id": map[string]any{"type": "string", "description": "Slack channel ID."},
					"text":       map[string]any{"type": "string", "description": "Message text."},
				},
				"required": []string{"channel_id", "text"},
			},
		},
	}

	server := mcplite.NewServer(tools, "ready")
	server.RegisterToolHandler("slack.status", func(_ context.Context, _ map[string]any) (string, error) {
		return marshalJSON(rt.status())
	})
	server.RegisterToolHandler("slack.link_state", func(_ context.Context, _ map[string]any) (string, error) {
		return marshalJSON(rt.linkState())
	})
	server.RegisterToolHandler("slack.send_message", func(_ context.Context, params map[string]any) (string, error) {
		channelID, _ := params["channel_id"].(string)
		text, _ := params["text"].(string)
		return rt.sendMessage(channelID, text)
	})
	return server
}

func handleConn(ctx context.Context, conn net.Conn, server *mcplite.Server, rt *slackRuntime) {
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

	writeEvent(mcplite.EventFrame{
		Type:  mcplite.TypeEvent,
		Event: "slack.connection.status",
		Data:  rt.status(),
	})

	done := make(chan struct{})
	go func() {
		ticker := time.NewTicker(40 * time.Millisecond)
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
				"service": "slack",
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
						"service": "slack",
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
					"service":    "slack",
					"request_id": requestID,
					"tool":       tool,
					"error":      err.Error(),
				})
				return
			}
			mcplite.LogEvent("INFO", "request handled", map[string]any{
				"service":     "slack",
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

func firstNonEmpty(values ...string) string {
	for _, v := range values {
		if v != "" {
			return v
		}
	}
	return ""
}
