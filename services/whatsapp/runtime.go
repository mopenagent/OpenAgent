package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"time"

	"github.com/kmaneesh/openagent/services/sdk-go/mcplite"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/store/sqlstore"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	"google.golang.org/protobuf/proto"
)

type waRuntime struct {
	dataDir      string
	artifactsDir string
	client       *whatsmeow.Client
	container    *sqlstore.Container

	started   atomic.Bool
	connected atomic.Bool

	lastQRMu sync.Mutex
	lastQR   string

	lastErrMu sync.Mutex
	lastErr   string

	// Buffered channel for outbound event frames to connected clients.
	// Capacity of 256 absorbs short bursts; overflow is logged and dropped.
	events chan mcplite.EventFrame
}

func newWaRuntime(dataDir, artifactsDir string) *waRuntime {
	if dataDir == "" {
		dataDir = defaultDataDir
	}
	if artifactsDir == "" {
		artifactsDir = defaultArtifactsDir
	}
	return &waRuntime{
		dataDir:      dataDir,
		artifactsDir: artifactsDir,
		events:       make(chan mcplite.EventFrame, 256),
	}
}

// emit pushes an event frame onto the channel; logs and drops on overflow.
func (r *waRuntime) emit(evt mcplite.EventFrame) {
	select {
	case r.events <- evt:
	default:
		mcplite.LogEvent("WARN", "event channel full — dropping event", map[string]any{
			"service": "whatsapp",
			"event":   evt.Event,
		})
	}
}

func (r *waRuntime) emitQR(qr string) {
	r.lastQRMu.Lock()
	r.lastQR = qr
	r.lastQRMu.Unlock()
	r.emit(mcplite.EventFrame{
		Type:  mcplite.TypeEvent,
		Event: "whatsapp.qr",
		Data:  map[string]any{"qr": qr},
	})
}

func (r *waRuntime) emitConnectionStatus() {
	data := map[string]any{
		"connected": r.connected.Load(),
		"backend":   "whatsmeow",
	}
	if errText := r.errorText(); errText != "" {
		data["last_error"] = errText
	}
	r.emit(mcplite.EventFrame{
		Type:  mcplite.TypeEvent,
		Event: "whatsapp.connection.status",
		Data:  data,
	})
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

	walog := waLogger{module: "whatsmeow"}
	container, err := sqlstore.New(ctx, "sqlite3", "file:"+dbPath+"?_foreign_keys=on", walog.Sub("sqlstore"))
	if err != nil {
		return fmt.Errorf("sqlstore: %w", err)
	}
	r.container = container

	deviceStore, err := container.GetFirstDevice(ctx)
	if err != nil {
		return fmt.Errorf("get device: %w", err)
	}

	client := whatsmeow.NewClient(deviceStore, walog.Sub("client"))
	r.client = client
	r.registerHandlers(client)

	r.started.Store(true)
	r.emitConnectionStatus()

	go r.connectLoop(ctx, client)
	return nil
}

// registerHandlers attaches all whatsmeow event handlers to the client.
func (r *waRuntime) registerHandlers(client *whatsmeow.Client) {
	client.AddEventHandler(func(rawEvt interface{}) {
		switch evt := rawEvt.(type) {

		case *events.Connected:
			r.connected.Store(true)
			r.setError("")
			r.lastQRMu.Lock()
			r.lastQR = ""
			r.lastQRMu.Unlock()
			r.emitConnectionStatus()

		case *events.Disconnected:
			r.connected.Store(false)
			r.emitConnectionStatus()

		case *events.LoggedOut:
			r.connected.Store(false)
			r.lastQRMu.Lock()
			r.lastQR = ""
			r.lastQRMu.Unlock()
			r.emitConnectionStatus()

		case *events.CallOffer:
			// 1:1 incoming call — media type not signalled in this event
			chatID := evt.From.ToNonAD().String()
			r.emit(mcplite.EventFrame{
				Type:  mcplite.TypeEvent,
				Event: "whatsapp.call.received",
				Data: map[string]any{
					"chat_id":   chatID,
					"sender":    chatID,
					"call_id":   evt.CallID,
					"is_video":  false,
					"call_type": "voice",
				},
			})

		case *events.CallOfferNotice:
			// Group call notice — Media is "audio" or "video"
			chatID := evt.From.ToNonAD().String()
			r.emit(mcplite.EventFrame{
				Type:  mcplite.TypeEvent,
				Event: "whatsapp.call.received",
				Data: map[string]any{
					"chat_id":   chatID,
					"sender":    chatID,
					"call_id":   evt.CallID,
					"is_video":  evt.Media == "video",
					"call_type": evt.Media,
				},
			})

		case *events.Message:
			r.handleMessage(evt)
		}
	})
}

func (r *waRuntime) handleMessage(evt *events.Message) {
	if evt.Info.IsFromMe || evt.Message == nil {
		return
	}
	chatID := evt.Info.Chat.String()
	sender := evt.Info.Sender.String()
	if sender == "" {
		sender = chatID
	}

	// Audio messages (PTT voice note or regular voice message) — download async
	if audioMsg := evt.Message.GetAudioMessage(); audioMsg != nil {
		isPTT := audioMsg.GetPTT()
		kind := "voice"
		if isPTT {
			kind = "ptt"
		}
		mcplite.LogEvent("INFO", "whatsapp audio received — downloading", map[string]any{
			"service": "whatsapp",
			"chat_id": chatID,
			"sender":  sender,
			"kind":    kind,
		})
		go func() {
			path, err := downloadAudio(r.client, audioMsg, r.artifactsDir, chatID)
			data := map[string]any{
				"chat_id": chatID,
				"sender":  sender,
				"kind":    kind,
				"is_ptt":  isPTT,
			}
			if err != nil {
				mcplite.LogEvent("ERROR", "audio download failed", map[string]any{
					"service": "whatsapp",
					"chat_id": chatID,
					"error":   err.Error(),
				})
				// Emit without artifact so agent still sees the event
				data["text"] = "[Voice message]"
			} else {
				data["artifact_path"] = path
				data["text"] = ""
				mcplite.LogEvent("INFO", "audio saved", map[string]any{
					"service": "whatsapp",
					"chat_id": chatID,
					"path":    path,
					"kind":    kind,
				})
			}
			r.emit(mcplite.EventFrame{
				Type:  mcplite.TypeEvent,
				Event: "whatsapp.message.received",
				Data:  data,
			})
		}()
		return
	}

	// Text / media messages
	text, kind := extractText(evt.Message)
	mcplite.LogEvent("INFO", "whatsapp message received", map[string]any{
		"service":  "whatsapp",
		"chat_id":  chatID,
		"sender":   sender,
		"kind":     kind,
		"has_text": text != "",
	})
	if text == "" {
		return
	}

	r.emit(mcplite.EventFrame{
		Type:  mcplite.TypeEvent,
		Event: "whatsapp.message.received",
		Data: map[string]any{
			"chat_id": chatID,
			"sender":  sender,
			"text":    text,
			"kind":    kind,
		},
	})
}

// connectLoop manages the whatsmeow connection lifecycle: QR pairing for new
// devices or direct connect for linked devices, with 5 s backoff on error.
func (r *waRuntime) connectLoop(ctx context.Context, client *whatsmeow.Client) {
	for {
		if ctx.Err() != nil {
			return
		}

		if client.Store.ID == nil {
			// Device not linked — need QR pairing
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

			if err = client.Connect(); err != nil {
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
				case "timeout":
					r.emitQR("")
					r.setError("QR timed out — please try again")
					r.emitConnectionStatus()
				}
			}
		} else {
			// Device linked — connect directly
			if err := client.Connect(); err != nil {
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
	s := map[string]any{
		"running":   r.started.Load(),
		"connected": r.connected.Load(),
		"backend":   "whatsmeow",
	}
	if errText := r.errorText(); errText != "" {
		s["last_error"] = errText
	}
	return s
}

func (r *waRuntime) linkState() map[string]any {
	return map[string]any{
		"connected": r.connected.Load(),
		"backend":   "whatsmeow",
	}
}

func (r *waRuntime) sendText(chatID, text string) (string, error) {
	if !r.started.Load() || r.client == nil {
		return "", fmt.Errorf("whatsapp runtime not started")
	}
	if !r.connected.Load() {
		return "", fmt.Errorf("whatsapp not connected — scan QR first")
	}
	if chatID == "" {
		return "", fmt.Errorf("chat_id is required")
	}
	if text == "" {
		return "", fmt.Errorf("text is required")
	}

	jid, err := types.ParseJID(chatID)
	if err != nil {
		return "", fmt.Errorf("invalid chat_id: %w", err)
	}

	msg := &waE2E.Message{Conversation: proto.String(text)}
	if _, err = r.client.SendMessage(context.Background(), jid, msg); err != nil {
		return "", err
	}

	return marshalJSON(map[string]any{"ok": true, "chat_id": chatID})
}
