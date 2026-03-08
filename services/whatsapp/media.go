package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waE2E"
)

// downloadAudio downloads and decrypts a WhatsApp audio message, saves it to
// artifactsDir/whatsapp/<filename>.<ext>, and returns the file path.
// The caller should run this in a goroutine — Download() does network I/O.
func downloadAudio(client *whatsmeow.Client, audioMsg *waE2E.AudioMessage, artifactsDir, chatID string) (string, error) {
	data, err := client.Download(context.Background(), audioMsg)
	if err != nil {
		return "", fmt.Errorf("download audio: %w", err)
	}

	ext := audioExt(audioMsg.GetMimetype())
	safeID := sanitiseID(chatID)
	filename := fmt.Sprintf("whatsapp-%s-%d%s", safeID, time.Now().UnixMilli(), ext)

	dir := filepath.Join(artifactsDir, "whatsapp")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return "", fmt.Errorf("create artifacts dir: %w", err)
	}

	path := filepath.Join(dir, filename)
	if err := os.WriteFile(path, data, 0o644); err != nil {
		return "", fmt.Errorf("write audio file: %w", err)
	}

	return path, nil
}

// audioExt maps a WhatsApp audio MIME type to a file extension.
// WhatsApp PTT is always OGG/Opus; regular voice notes may vary.
func audioExt(mimeType string) string {
	// Strip codec parameters: "audio/ogg; codecs=opus" → "audio/ogg"
	if idx := strings.IndexByte(mimeType, ';'); idx != -1 {
		mimeType = strings.TrimSpace(mimeType[:idx])
	}
	switch strings.ToLower(mimeType) {
	case "audio/ogg", "audio/opus":
		return ".ogg"
	case "audio/mpeg", "audio/mp3":
		return ".mp3"
	case "audio/mp4", "audio/m4a", "audio/aac":
		return ".m4a"
	default:
		return ".ogg" // WhatsApp default
	}
}

// sanitiseID produces a filename-safe string from a WhatsApp JID like
// "919876543210@s.whatsapp.net" → "919876543210_s_whatsapp_net"
func sanitiseID(id string) string {
	r := strings.NewReplacer("@", "_", ".", "_", ":", "_", "/", "_", "+", "")
	s := r.Replace(id)
	if len(s) > 32 {
		s = s[:32]
	}
	return s
}
