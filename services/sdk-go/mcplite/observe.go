package mcplite

import (
	"encoding/json"
	"log"
	"time"
)

// LogEvent emits one structured JSON log line.
func LogEvent(level string, message string, fields map[string]any) {
	payload := map[string]any{
		"ts":      time.Now().UTC().Format(time.RFC3339Nano),
		"level":   level,
		"message": message,
	}
	for k, v := range fields {
		payload[k] = v
	}
	data, err := json.Marshal(payload)
	if err != nil {
		log.Printf("{\"ts\":%q,\"level\":\"ERROR\",\"message\":\"log marshal failed\",\"error\":%q}", time.Now().UTC().Format(time.RFC3339Nano), err.Error())
		return
	}
	log.Print(string(data))
}

// RequestIDFromFrame extracts the correlation id when available.
func RequestIDFromFrame(frame Frame) string {
	switch v := frame.(type) {
	case ToolListRequest:
		return v.ID
	case ToolCallRequest:
		return v.ID
	case PingRequest:
		return v.ID
	default:
		return ""
	}
}

// ToolNameFromFrame extracts tool name from tool.call frames.
func ToolNameFromFrame(frame Frame) string {
	switch v := frame.(type) {
	case ToolCallRequest:
		return v.Tool
	default:
		return ""
	}
}
