package main

import (
	"context"
	"fmt"
	"mime"
	"os"
	"path/filepath"
	"strings"
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

	// disconnected is pulsed (non-blocking send) whenever WhatsApp fires a
	// Disconnected or LoggedOut event so connectLoop can reconnect promptly.
	disconnected chan struct{}
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
		disconnected: make(chan struct{}, 1),
	}
}

// signalDisconnected notifies connectLoop that WhatsApp dropped the connection.
func (r *waRuntime) signalDisconnected() {
	select {
	case r.disconnected <- struct{}{}:
	default: // already pending, no need to queue another
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
			r.signalDisconnected()

		case *events.LoggedOut:
			r.connected.Store(false)
			r.lastQRMu.Lock()
			r.lastQR = ""
			r.lastQRMu.Unlock()
			r.emitConnectionStatus()
			r.signalDisconnected()

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
	chatID := evt.Info.Chat.ToNonAD().String()
	// ToNonAD strips the device number from multi-device JIDs:
	// "916356737267:11@s.whatsapp.net" → "916356737267@s.whatsapp.net"
	// This gives a stable sender ID for whitelist/session lookups.
	sender := evt.Info.Sender.ToNonAD().String()
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
				"channel": "whatsapp://" + chatID,
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
				data["content"] = "[Voice message]"
			} else {
				data["artifact_path"] = path
				data["content"] = ""
				mcplite.LogEvent("INFO", "audio saved", map[string]any{
					"service": "whatsapp",
					"chat_id": chatID,
					"path":    path,
					"kind":    kind,
				})
			}
			r.emit(mcplite.EventFrame{
				Type:  mcplite.TypeEvent,
				Event: "message.received",
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
		Event: "message.received",
		Data: map[string]any{
			"channel": "whatsapp://" + chatID,
			"sender":  sender,
			"content": text,
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

			// Wait for either a clean shutdown or a server-side disconnect.
			select {
			case <-ctx.Done():
				client.Disconnect()
				return
			case <-r.disconnected:
				// WhatsApp dropped the connection — reconnect after backoff.
				mcplite.LogEvent("INFO", "whatsapp disconnected — reconnecting in 5s", map[string]any{
					"service": "whatsapp",
				})
				select {
				case <-ctx.Done():
					return
				case <-time.After(5 * time.Second):
				}
				continue
			}
		}
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

// sendTyping sends a composing presence indicator to a chat.
// WhatsApp clears the typing indicator automatically after ~10 s.
func (r *waRuntime) sendTyping(chatID string) (string, error) {
	if !r.started.Load() || r.client == nil {
		return "", fmt.Errorf("whatsapp runtime not started")
	}
	if !r.connected.Load() {
		return "", fmt.Errorf("whatsapp not connected")
	}
	if chatID == "" {
		return "", fmt.Errorf("chat_id is required")
	}
	jid, err := types.ParseJID(chatID)
	if err != nil {
		return "", fmt.Errorf("invalid chat_id: %w", err)
	}
	if err := r.client.SendChatPresence(jid, types.ChatPresenceComposing, types.ChatPresenceMediaText); err != nil {
		return "", fmt.Errorf("send typing: %w", err)
	}
	return marshalJSON(map[string]any{"ok": true})
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

// sendMedia uploads a local file and sends it as an image, video, audio, or
// document message depending on MIME type.
//
// Parameters:
//
//	chatID   — destination JID (e.g. "919876543210@s.whatsapp.net")
//	filePath — absolute path to a local file
//	mimeType — MIME type string; empty = detected from file extension
//	caption  — optional caption (shown under images / videos / documents)
func (r *waRuntime) sendMedia(chatID, filePath, mimeType, caption string) (string, error) {
	if !r.started.Load() || r.client == nil {
		return "", fmt.Errorf("whatsapp runtime not started")
	}
	if !r.connected.Load() {
		return "", fmt.Errorf("whatsapp not connected — scan QR first")
	}
	if chatID == "" {
		return "", fmt.Errorf("chat_id is required")
	}
	if filePath == "" {
		return "", fmt.Errorf("file_path is required")
	}

	data, err := os.ReadFile(filePath)
	if err != nil {
		return "", fmt.Errorf("read file: %w", err)
	}

	// Detect MIME type from extension if not provided.
	if mimeType == "" {
		mimeType = mime.TypeByExtension(strings.ToLower(filepath.Ext(filePath)))
		if mimeType == "" {
			mimeType = "application/octet-stream"
		}
	}

	jid, err := types.ParseJID(chatID)
	if err != nil {
		return "", fmt.Errorf("invalid chat_id: %w", err)
	}

	// Choose whatsmeow media type from MIME prefix.
	mediaType, waMsg := mediaTypeForMIME(mimeType, data, caption, filePath)

	uploaded, err := r.client.Upload(context.Background(), data, mediaType)
	if err != nil {
		return "", fmt.Errorf("upload media: %w", err)
	}

	// Fill in the upload envelope fields that whatsmeow needs.
	fillUploadFields(waMsg, &uploaded, mimeType, uint64(len(data)))

	if _, err = r.client.SendMessage(context.Background(), jid, &waE2E.Message{
		ImageMessage:    waMsg.image,
		VideoMessage:    waMsg.video,
		AudioMessage:    waMsg.audio,
		DocumentMessage: waMsg.document,
	}); err != nil {
		return "", fmt.Errorf("send media: %w", err)
	}

	return marshalJSON(map[string]any{
		"ok":        true,
		"chat_id":   chatID,
		"file_path": filePath,
		"mime_type": mimeType,
	})
}

// mediaMsg holds exactly one of the four whatsmeow media message types.
type mediaMsg struct {
	image    *waE2E.ImageMessage
	video    *waE2E.VideoMessage
	audio    *waE2E.AudioMessage
	document *waE2E.DocumentMessage
}

// mediaTypeForMIME returns the whatsmeow upload type and a skeleton message
// proto for the given MIME type.
func mediaTypeForMIME(mimeType, _ []byte, caption, filePath string) (whatsmeow.MediaType, mediaMsg) {
	base := strings.SplitN(mimeType, "/", 2)[0]
	switch base {
	case "image":
		return whatsmeow.MediaImage, mediaMsg{
			image: &waE2E.ImageMessage{
				Mimetype: proto.String(mimeType),
				Caption:  proto.String(caption),
			},
		}
	case "video":
		return whatsmeow.MediaVideo, mediaMsg{
			video: &waE2E.VideoMessage{
				Mimetype: proto.String(mimeType),
				Caption:  proto.String(caption),
			},
		}
	case "audio":
		return whatsmeow.MediaAudio, mediaMsg{
			audio: &waE2E.AudioMessage{
				Mimetype: proto.String(mimeType),
				PTT:      proto.Bool(false),
			},
		}
	default:
		// Documents — includes PDF, ZIP, etc.
		filename := filepath.Base(filePath)
		return whatsmeow.MediaDocument, mediaMsg{
			document: &waE2E.DocumentMessage{
				Mimetype: proto.String(mimeType),
				Caption:  proto.String(caption),
				FileName: proto.String(filename),
			},
		}
	}
}

// fillUploadFields copies the upload response envelope fields into the
// appropriate message proto.
func fillUploadFields(m *mediaMsg, u *whatsmeow.UploadResponse, _ string, fileLen uint64) {
	switch {
	case m.image != nil:
		m.image.URL = proto.String(u.URL)
		m.image.DirectPath = proto.String(u.DirectPath)
		m.image.MediaKey = u.MediaKey
		m.image.FileEncSHA256 = u.FileEncSHA256
		m.image.FileSHA256 = u.FileSHA256
		m.image.FileLength = proto.Uint64(fileLen)
	case m.video != nil:
		m.video.URL = proto.String(u.URL)
		m.video.DirectPath = proto.String(u.DirectPath)
		m.video.MediaKey = u.MediaKey
		m.video.FileEncSHA256 = u.FileEncSHA256
		m.video.FileSHA256 = u.FileSHA256
		m.video.FileLength = proto.Uint64(fileLen)
	case m.audio != nil:
		m.audio.URL = proto.String(u.URL)
		m.audio.DirectPath = proto.String(u.DirectPath)
		m.audio.MediaKey = u.MediaKey
		m.audio.FileEncSHA256 = u.FileEncSHA256
		m.audio.FileSHA256 = u.FileSHA256
		m.audio.FileLength = proto.Uint64(fileLen)
	case m.document != nil:
		m.document.URL = proto.String(u.URL)
		m.document.DirectPath = proto.String(u.DirectPath)
		m.document.MediaKey = u.MediaKey
		m.document.FileEncSHA256 = u.FileEncSHA256
		m.document.FileSHA256 = u.FileSHA256
		m.document.FileLength = proto.Uint64(fileLen)
	}
}
