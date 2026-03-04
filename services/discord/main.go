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

	"github.com/bwmarrin/discordgo"
	"github.com/kmaneesh/openagent/services/sdk-go/mcplite"
)

const defaultSocketPath = "data/sockets/discord.sock"

type discordRuntime struct {
	token string
	sess  *discordgo.Session

	started    atomic.Bool
	connected  atomic.Bool
	authorized atomic.Bool

	lastErrMu sync.Mutex
	lastErr   string

	events chan mcplite.EventFrame
}

func newDiscordRuntime(token string) *discordRuntime {
	return &discordRuntime{
		token:  token,
		events: make(chan mcplite.EventFrame, 256),
	}
}

func (r *discordRuntime) start() error {
	if r.started.Load() {
		return nil
	}
	if r.token == "" {
		return errors.New("missing DISCORD_BOT_TOKEN or OPENAGENT_DISCORD_BOT_TOKEN")
	}

	session, err := discordgo.New("Bot " + r.token)
	if err != nil {
		return err
	}
	session.Identify.Intents = discordgo.IntentsGuildMessages |
		discordgo.IntentsDirectMessages |
		discordgo.IntentsMessageContent

	session.AddHandler(func(_ *discordgo.Session, _ *discordgo.Ready) {
		r.connected.Store(true)
		r.authorized.Store(true)
		r.setError("")
		r.emitConnectionStatus()
	})
	session.AddHandler(func(_ *discordgo.Session, m *discordgo.MessageCreate) {
		if m == nil || m.Author == nil {
			return
		}
		select {
		case r.events <- mcplite.EventFrame{
			Type:  mcplite.TypeEvent,
			Event: "discord.message.received",
			Data: map[string]any{
				"id":         m.ID,
				"channel_id": m.ChannelID,
				"guild_id":   m.GuildID,
				"author_id":  m.Author.ID,
				"author":     m.Author.Username,
				"content":    m.Content,
				"is_bot":     m.Author.Bot,
			},
		}:
		default:
		}
	})

	if err := session.Open(); err != nil {
		return err
	}

	r.sess = session
	r.started.Store(true)
	// Ready event may arrive shortly after Open; publish a baseline state now.
	r.emitConnectionStatus()
	return nil
}

func (r *discordRuntime) stop() {
	if !r.started.Load() {
		return
	}
	if r.sess != nil {
		_ = r.sess.Close()
	}
	r.started.Store(false)
	r.connected.Store(false)
	r.authorized.Store(false)
	r.emitConnectionStatus()
}

func (r *discordRuntime) status() map[string]any {
	status := map[string]any{
		"running":    r.started.Load(),
		"connected":  r.connected.Load(),
		"authorized": r.authorized.Load(),
		"backend":    "discordgo",
	}
	if msg := r.errorText(); msg != "" {
		status["last_error"] = msg
	}
	return status
}

func (r *discordRuntime) linkState() map[string]any {
	return map[string]any{
		"authorized": r.authorized.Load(),
		"connected":  r.connected.Load(),
		"backend":    "discordgo",
	}
}

func (r *discordRuntime) sendMessage(channelID, text string) (string, error) {
	if channelID == "" {
		return "", errors.New("channel_id is required")
	}
	if text == "" {
		return "", errors.New("text is required")
	}
	if !r.started.Load() || r.sess == nil {
		return "", errors.New("discord runtime is not started")
	}
	msg, err := r.sess.ChannelMessageSend(channelID, text)
	if err != nil {
		r.setError(err.Error())
		r.emitConnectionStatus()
		return "", err
	}
	return marshalJSON(map[string]any{
		"ok":         true,
		"id":         msg.ID,
		"channel_id": msg.ChannelID,
	})
}

func (r *discordRuntime) pollEvent() (mcplite.EventFrame, bool) {
	select {
	case evt := <-r.events:
		return evt, true
	default:
		return mcplite.EventFrame{}, false
	}
}

func (r *discordRuntime) emitConnectionStatus() {
	data := map[string]any{
		"connected":  r.connected.Load(),
		"authorized": r.authorized.Load(),
		"backend":    "discordgo",
	}
	if msg := r.errorText(); msg != "" {
		data["last_error"] = msg
	}
	select {
	case r.events <- mcplite.EventFrame{
		Type:  mcplite.TypeEvent,
		Event: "discord.connection.status",
		Data:  data,
	}:
	default:
	}
}

func (r *discordRuntime) setError(msg string) {
	r.lastErrMu.Lock()
	defer r.lastErrMu.Unlock()
	r.lastErr = msg
}

func (r *discordRuntime) errorText() string {
	r.lastErrMu.Lock()
	defer r.lastErrMu.Unlock()
	return r.lastErr
}

func main() {
	if err := run(); err != nil {
		log.Fatalf("discord service failed: %v", err)
	}
}

func run() error {
	ctx, cancel := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer cancel()

	socketPath := os.Getenv("OPENAGENT_SOCKET_PATH")
	if socketPath == "" {
		socketPath = defaultSocketPath
	}
	token := firstNonEmpty(
		os.Getenv("DISCORD_BOT_TOKEN"),
		os.Getenv("OPENAGENT_DISCORD_BOT_TOKEN"),
	)

	runtime := newDiscordRuntime(token)
	if err := runtime.start(); err != nil {
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
		"service":     "discord",
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
				"service": "discord",
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

func buildServer(rt *discordRuntime) *mcplite.Server {
	tools := []mcplite.ToolDefinition{
		{
			Name:        "discord.status",
			Description: "Return current Discord service status.",
			Params:      map[string]any{"type": "object", "properties": map[string]any{}},
		},
		{
			Name:        "discord.link_state",
			Description: "Return Discord connection and auth state.",
			Params:      map[string]any{"type": "object", "properties": map[string]any{}},
		},
		{
			Name:        "discord.send_message",
			Description: "Send message to a Discord channel.",
			Params: map[string]any{
				"type": "object",
				"properties": map[string]any{
					"channel_id": map[string]any{
						"type":        "string",
						"description": "Discord channel ID.",
					},
					"text": map[string]any{
						"type":        "string",
						"description": "Message text.",
					},
				},
				"required": []string{"channel_id", "text"},
			},
		},
	}

	server := mcplite.NewServer(tools, "ready")
	server.RegisterToolHandler("discord.status", func(_ context.Context, _ map[string]any) (string, error) {
		return marshalJSON(rt.status())
	})
	server.RegisterToolHandler("discord.link_state", func(_ context.Context, _ map[string]any) (string, error) {
		return marshalJSON(rt.linkState())
	})
	server.RegisterToolHandler("discord.send_message", func(_ context.Context, params map[string]any) (string, error) {
		channelID, _ := params["channel_id"].(string)
		text, _ := params["text"].(string)
		return rt.sendMessage(channelID, text)
	})
	return server
}

func handleConn(ctx context.Context, conn net.Conn, server *mcplite.Server, rt *discordRuntime) {
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
		Event: "discord.connection.status",
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
				"service": "discord",
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
						"service": "discord",
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
					"service":    "discord",
					"request_id": requestID,
					"tool":       tool,
					"error":      err.Error(),
				})
				return
			}
			mcplite.LogEvent("INFO", "request handled", map[string]any{
				"service":     "discord",
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
