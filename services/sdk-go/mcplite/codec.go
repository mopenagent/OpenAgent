package mcplite

import (
	"bufio"
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
)

const scannerMaxTokenBytes = 1024 * 1024

type frameHeader struct {
	Type string `json:"type"`
}

// DecodeFrame parses one JSON frame and returns the concrete protocol struct.
func DecodeFrame(line []byte) (Frame, error) {
	trimmed := bytes.TrimSpace(line)
	if len(trimmed) == 0 {
		return nil, errors.New("empty frame")
	}

	var header frameHeader
	if err := json.Unmarshal(trimmed, &header); err != nil {
		return nil, fmt.Errorf("decode frame header: %w", err)
	}

	switch header.Type {
	case TypeToolsList:
		var f ToolListRequest
		if err := json.Unmarshal(trimmed, &f); err != nil {
			return nil, fmt.Errorf("decode tools.list: %w", err)
		}
		return f, nil
	case TypeToolCall:
		var f ToolCallRequest
		if err := json.Unmarshal(trimmed, &f); err != nil {
			return nil, fmt.Errorf("decode tool.call: %w", err)
		}
		return f, nil
	case TypePing:
		var f PingRequest
		if err := json.Unmarshal(trimmed, &f); err != nil {
			return nil, fmt.Errorf("decode ping: %w", err)
		}
		return f, nil
	case TypeToolsListOK:
		var f ToolListResponse
		if err := json.Unmarshal(trimmed, &f); err != nil {
			return nil, fmt.Errorf("decode tools.list.ok: %w", err)
		}
		return f, nil
	case TypeToolResult:
		var f ToolResultResponse
		if err := json.Unmarshal(trimmed, &f); err != nil {
			return nil, fmt.Errorf("decode tool.result: %w", err)
		}
		return f, nil
	case TypePong:
		var f PongResponse
		if err := json.Unmarshal(trimmed, &f); err != nil {
			return nil, fmt.Errorf("decode pong: %w", err)
		}
		return f, nil
	case TypeError:
		var f ErrorResponse
		if err := json.Unmarshal(trimmed, &f); err != nil {
			return nil, fmt.Errorf("decode error: %w", err)
		}
		return f, nil
	case TypeEvent:
		var f EventFrame
		if err := json.Unmarshal(trimmed, &f); err != nil {
			return nil, fmt.Errorf("decode event: %w", err)
		}
		return f, nil
	default:
		return nil, fmt.Errorf("unsupported frame type %q", header.Type)
	}
}

// EncodeFrame serializes one frame and appends a newline as protocol delimiter.
func EncodeFrame(frame Frame) ([]byte, error) {
	data, err := json.Marshal(normalizeFrame(frame))
	if err != nil {
		return nil, fmt.Errorf("encode frame: %w", err)
	}
	return append(data, '\n'), nil
}

func normalizeFrame(frame Frame) Frame {
	switch f := frame.(type) {
	case ToolCallRequest:
		if f.Params == nil {
			f.Params = map[string]any{}
		}
		return f
	case EventFrame:
		if f.Data == nil {
			f.Data = map[string]any{}
		}
		return f
	default:
		return frame
	}
}

// Decoder reads newline-delimited MCP-lite frames from a stream.
type Decoder struct {
	scanner *bufio.Scanner
}

// NewDecoder builds a decoder with an increased scanner buffer.
func NewDecoder(r io.Reader) *Decoder {
	scanner := bufio.NewScanner(r)
	scanner.Buffer(make([]byte, 0, 16*1024), scannerMaxTokenBytes)
	return &Decoder{scanner: scanner}
}

// Next returns the next protocol frame. io.EOF signals stream completion.
func (d *Decoder) Next() (Frame, error) {
	if !d.scanner.Scan() {
		if err := d.scanner.Err(); err != nil {
			return nil, err
		}
		return nil, io.EOF
	}
	return DecodeFrame(d.scanner.Bytes())
}

// Encoder writes newline-delimited MCP-lite frames to a stream.
type Encoder struct {
	w io.Writer
}

func NewEncoder(w io.Writer) *Encoder {
	return &Encoder{w: w}
}

// WriteFrame encodes and writes one frame.
func (e *Encoder) WriteFrame(frame Frame) error {
	data, err := EncodeFrame(frame)
	if err != nil {
		return err
	}
	_, err = e.w.Write(data)
	return err
}
