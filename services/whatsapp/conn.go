package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"sync"
	"time"

	"github.com/kmaneesh/openagent/services/sdk-go/mcplite"
)

// handleConn manages a single MCP-lite client connection lifecycle:
//   - Sends initial connection status and any pending QR code on connect
//   - Forwards runtime events to the client as they arrive (no polling)
//   - Dispatches inbound request frames to the server concurrently
func handleConn(ctx context.Context, conn net.Conn, server *mcplite.Server, rt *waRuntime) {
	defer conn.Close()

	decoder := mcplite.NewDecoder(conn)
	encoder := mcplite.NewEncoder(conn)
	var writeMu sync.Mutex
	var reqWG sync.WaitGroup

	writeFrame := func(frame mcplite.Frame) {
		writeMu.Lock()
		defer writeMu.Unlock()
		if err := encoder.WriteFrame(frame); err != nil {
			log.Printf("write frame failed: %v", err)
		}
	}

	// Send current state immediately so the Python client is never blind
	writeFrame(mcplite.EventFrame{
		Type:  mcplite.TypeEvent,
		Event: "whatsapp.connection.status",
		Data:  rt.status(),
	})
	if qr := rt.latestQR(); qr != "" {
		writeFrame(mcplite.EventFrame{
			Type:  mcplite.TypeEvent,
			Event: "whatsapp.qr",
			Data:  map[string]any{"qr": qr},
		})
	}

	// Forward runtime events to the socket as soon as they arrive
	done := make(chan struct{})
	go func() {
		for {
			select {
			case <-done:
				return
			case evt := <-rt.events:
				writeFrame(evt)
			}
		}
	}()

	// Read and dispatch inbound request frames
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
					mcplite.LogEvent("WARN", "unsupported frame type", map[string]any{
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
