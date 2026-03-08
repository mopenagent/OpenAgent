// Package main implements the WhatsApp MCP-lite service for OpenAgent.
//
// File layout:
//   main.go    — entry point: main(), run(), env helpers
//   runtime.go — waRuntime: whatsmeow lifecycle, event handling
//   conn.go    — handleConn: per-client MCP-lite connection
//   server.go  — buildServer: tool definitions and handlers
//   extract.go — extractText: multi-type WhatsApp message extraction
//   media.go   — downloadAudio: artifact download + MIME helpers
//   logger.go  — waLogger: whatsmeow → mcplite log bridge
package main

import (
	"context"
	"errors"
	"log"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"sync"
	"syscall"

	"github.com/kmaneesh/openagent/services/sdk-go/mcplite"
	_ "github.com/mattn/go-sqlite3" // sqlite3 driver for whatsmeow sqlstore
)

const (
	defaultSocketPath   = "data/sockets/whatsapp.sock"
	defaultDataDir      = "data"
	defaultArtifactsDir = "data/artifacts"
)

func main() {
	if err := run(); err != nil {
		log.Fatalf("whatsapp service failed: %v", err)
	}
}

func run() error {
	ctx, cancel := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer cancel()

	socketPath := firstNonEmpty(os.Getenv("OPENAGENT_SOCKET_PATH"), defaultSocketPath)
	dataDir := firstNonEmpty(
		os.Getenv("WHATSAPP_DATA_DIR"),
		os.Getenv("OPENAGENT_WHATSAPP_DATA_DIR"),
		defaultDataDir,
	)
	artifactsDir := firstNonEmpty(
		os.Getenv("OPENAGENT_ARTIFACTS_DIR"),
		defaultArtifactsDir,
	)

	rt := newWaRuntime(dataDir, artifactsDir)
	if err := rt.start(ctx); err != nil {
		return err
	}
	defer rt.stop()

	if err := os.MkdirAll(filepath.Dir(socketPath), 0o755); err != nil {
		return errors.New("create socket directory: " + err.Error())
	}
	if err := os.Remove(socketPath); err != nil && !errors.Is(err, os.ErrNotExist) {
		return errors.New("remove stale socket: " + err.Error())
	}

	listener, err := net.Listen("unix", socketPath)
	if err != nil {
		return errors.New("listen on socket: " + err.Error())
	}
	defer func() {
		_ = listener.Close()
		_ = os.Remove(socketPath)
	}()

	mcplite.LogEvent("INFO", "service listening", map[string]any{
		"service":     "whatsapp",
		"socket_path": socketPath,
	})

	server := buildServer(rt)
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
			handleConn(ctx, c, server, rt)
		}(conn)
	}

	connWG.Wait()
	return nil
}

func firstNonEmpty(values ...string) string {
	for _, v := range values {
		if strings.TrimSpace(v) != "" {
			return v
		}
	}
	return ""
}
