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
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"time"

	_ "github.com/mattn/go-sqlite3" // register sqlite3 driver for whatsmeow sqlstore
	"github.com/kmaneesh/openagent/services/sdk-go/mcplite"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/store/sqlstore"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	waLog "go.mau.fi/whatsmeow/util/log"
	"google.golang.org/protobuf/proto"
)

const defaultSocketPath = "data/sockets/whatsapp.sock"
const defaultDataDir = "data"

type waRuntime struct {
	dataDir   string
	client    *whatsmeow.Client
	container *sqlstore.Container

	started   atomic.Bool
	connected atomic.Bool

	lastQRMu sync.Mutex
	lastQR   string

	lastErrMu sync.Mutex
	lastErr   string

	events chan mcplite.EventFrame
}

func firstNonEmpty(values ...string) string {
	for _, v := range values {
		if strings.TrimSpace(v) != "" {
			return v
		}
	}
	return ""
}

func newWaRuntime(dataDir string) *waRuntime {
	if dataDir == "" {
		dataDir = defaultDataDir
	}
	return &waRuntime{
		dataDir: dataDir,
		events:  make(chan mcplite.EventFrame, 128),
	}
}

func (r *waRuntime) emitQR(qr string) {
	r.lastQRMu.Lock()
	r.lastQR = qr
	r.lastQRMu.Unlock()
	select {
	case r.events <- mcplite.EventFrame{
		Type:  mcplite.TypeEvent,
		Event: "whatsapp.qr",
		Data:  map[string]any{"qr": qr},
	}:
	default:
	}
}

func (r *waRuntime) emitConnectionStatus() {
	data := map[string]any{
		"connected": r.connected.Load(),
		"backend":   "whatsmeow",
	}
	if errText := r.errorText(); errText != "" {
		data["last_error"] = errText
	}
	select {
	case r.events <- mcplite.EventFrame{
		Type:  mcplite.TypeEvent,
		Event: "whatsapp.connection.status",
		Data:  data,
	}:
	default:
	}
}

func (r *waRuntime) setError(msg string) {
	r.lastErrMu.Lock()
	defer r.lastErrMu.Unlock()
	r.lastErr = msg
}

func (r *waRuntime) errorText() string {
	r.lastErrMu.Lock()
	defer r.lastErrMu.Unlock()
	return r.lastErr
}

func (r *waRuntime) latestQR() string {
	r.lastQRMu.Lock()
	defer r.lastQRMu.Unlock()
	return r.lastQR
}

func (r *waRuntime) start(ctx context.Context) error {
	if r.started.Load() {
		return nil
	}

	dbPath := filepath.Join(r.dataDir, "whatsapp.db")
	if err := os.MkdirAll(filepath.Dir(dbPath), 0o755); err != nil {
		return fmt.Errorf("create data dir: %w", err)
	}
	container, err := sqlstore.New(ctx, "sqlite3", "file:"+dbPath+"?_foreign_keys=on", waLog.Noop)
	if err != nil {
		return fmt.Errorf("sqlstore: %w", err)
	}
	r.container = container

	deviceStore, err := container.GetFirstDevice(ctx)
	if err != nil {
		return fmt.Errorf("get device: %w", err)
	}

	client := whatsmeow.NewClient(deviceStore, waLog.Noop)
	r.client = client

	client.AddEventHandler(func(rawEvt interface{}) {
		switch evt := rawEvt.(type) {
		case *events.Connected:
			r.connected.Store(true)
			r.setError("")
			r.emitConnectionStatus()
			// Clear QR when connected
			r.lastQRMu.Lock()
			r.lastQR = ""
			r.lastQRMu.Unlock()
		case *events.Disconnected:
			r.connected.Store(false)
			r.emitConnectionStatus()
		case *events.LoggedOut:
			r.connected.Store(false)
			r.lastQRMu.Lock()
			r.lastQR = ""
			r.lastQRMu.Unlock()
			r.emitConnectionStatus()
		case *events.Message:
			if evt.Info.IsFromMe || evt.Message == nil {
				return
			}
			chatID := evt.Info.Chat.String()
			text := evt.Message.GetConversation()
			if text == "" {
				return
			}
			sender := evt.Info.Sender.String()
			if sender == "" {
				sender = chatID
			}
			select {
			case r.events <- mcplite.EventFrame{
				Type:  mcplite.TypeEvent,
				Event: "whatsapp.message.received",
				Data: map[string]any{
					"chat_id": chatID,
					"sender":  sender,
					"text":    text,
				},
			}:
			default:
			}
		}
	})

	r.started.Store(true)
	r.emitConnectionStatus()

	go func() {
		for {
			select {
			case <-ctx.Done():
				return
			default:
			}

			if client.Store.ID == nil {
				qrChan, err := client.GetQRChannel(ctx)
				if err != nil {
					r.setError(fmt.Sprintf("get QR channel: %v", err))
					r.emitConnectionStatus()
					select {
					case <-ctx.Done():
						return
					case <-time.After(5 * time.Second):
					}
					continue
				}

				err = client.Connect()
				if err != nil {
					r.setError(fmt.Sprintf("connect: %v", err))
					r.emitConnectionStatus()
					select {
					case <-ctx.Done():
						return
					case <-time.After(5 * time.Second):
					}
					continue
				}

				for evt := range qrChan {
					if ctx.Err() != nil {
						return
					}
					switch evt.Event {
					case "code":
						r.emitQR(evt.Code)
					case "success":
						r.emitQR("")
						break
					case "timeout":
						r.emitQR("")
						r.setError("QR timed out — please try again")
						r.emitConnectionStatus()
					}
				}
			} else {
				err := client.Connect()
				if err != nil {
					r.setError(fmt.Sprintf("connect: %v", err))
					r.emitConnectionStatus()
					select {
					case <-ctx.Done():
						return
					case <-time.After(5 * time.Second):
					}
					continue
				}
			}

			<-ctx.Done()
			client.Disconnect()
			return
		}
	}()

	return nil
}

func (r *waRuntime) stop() {
	r.started.Store(false)
	r.connected.Store(false)
	if r.client != nil {
		r.client.Disconnect()
	}
	r.emitConnectionStatus()
}

func (r *waRuntime) status() map[string]any {
	status := map[string]any{
		"running":   r.started.Load(),
		"connected": r.connected.Load(),
		"backend":   "whatsmeow",
	}
	if errText := r.errorText(); errText != "" {
		status["last_error"] = errText
	}
	return status
}

func (r *waRuntime) linkState() map[string]any {
	return map[string]any{
		"connected": r.connected.Load(),
		"backend":   "whatsmeow",
	}
}

func (r *waRuntime) sendText(chatID, text string) (string, error) {
	if !r.started.Load() || r.client == nil {
		return "", errors.New("whatsapp runtime not started")
	}
	if !r.connected.Load() {
		return "", errors.New("whatsapp not connected — scan QR first")
	}
	if chatID == "" {
		return "", errors.New("chat_id is required")
	}
	if text == "" {
		return "", errors.New("text is required")
	}

	jid, err := types.ParseJID(chatID)
	if err != nil {
		return "", fmt.Errorf("invalid chat_id: %w", err)
	}

	msg := &waE2E.Message{Conversation: proto.String(text)}
	_, err = r.client.SendMessage(context.Background(), jid, msg)
	if err != nil {
		return "", err
	}

	return compactJSON(map[string]any{
		"ok":      true,
		"chat_id": chatID,
	})
}

func (r *waRuntime) pollEvent() (mcplite.EventFrame, bool) {
	select {
	case evt := <-r.events:
		return evt, true
	default:
		return mcplite.EventFrame{}, false
	}
}

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

	dataDir := firstNonEmpty(
		os.Getenv("WHATSAPP_DATA_DIR"),
		os.Getenv("OPENAGENT_WHATSAPP_DATA_DIR"),
		defaultDataDir,
	)

	runtime := newWaRuntime(dataDir)
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
		"service":     "whatsapp",
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
				"service": "whatsapp",
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

func buildServer(rt *waRuntime) *mcplite.Server {
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
					"chat_id": map[string]any{"type": "string", "description": "Destination chat ID/JID (e.g. 15551234567@s.whatsapp.net)."},
					"text":    map[string]any{"type": "string", "description": "Text message to send."},
				},
				"required": []string{"chat_id", "text"},
			},
		},
	}

	server := mcplite.NewServer(tools, "ready")
	server.RegisterToolHandler("whatsapp.status", func(_ context.Context, _ map[string]any) (string, error) {
		return marshalJSON(rt.status())
	})
	server.RegisterToolHandler("whatsapp.link_state", func(_ context.Context, _ map[string]any) (string, error) {
		return marshalJSON(rt.linkState())
	})
	server.RegisterToolHandler("whatsapp.send_text", func(_ context.Context, params map[string]any) (string, error) {
		chatID, _ := params["chat_id"].(string)
		text, _ := params["text"].(string)
		return rt.sendText(chatID, text)
	})
	return server
}

func handleConn(ctx context.Context, conn net.Conn, server *mcplite.Server, rt *waRuntime) {
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
		Event: "whatsapp.connection.status",
		Data:  rt.status(),
	})
	if qr := rt.latestQR(); qr != "" {
		writeEvent(mcplite.EventFrame{
			Type:  mcplite.TypeEvent,
			Event: "whatsapp.qr",
			Data:  map[string]any{"qr": qr},
		})
	}

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

func compactJSON(v map[string]any) (string, error) {
	return marshalJSON(v)
}
