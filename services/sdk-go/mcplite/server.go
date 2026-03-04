package mcplite

import (
	"context"
	"fmt"
)

// ToolHandler executes one tool call.
type ToolHandler func(ctx context.Context, params map[string]any) (string, error)

// Server provides request dispatch with protocol-correct responses.
type Server struct {
	tools    []ToolDefinition
	handlers map[string]ToolHandler
	status   string
}

// NewServer creates a request dispatcher for MCP-lite services.
func NewServer(tools []ToolDefinition, status string) *Server {
	if status == "" {
		status = "ready"
	}
	return &Server{
		tools:    tools,
		handlers: make(map[string]ToolHandler, len(tools)),
		status:   status,
	}
}

// RegisterToolHandler binds a tool name to its execution function.
func (s *Server) RegisterToolHandler(name string, handler ToolHandler) {
	s.handlers[name] = handler
}

// HandleRequest routes incoming request frames.
func (s *Server) HandleRequest(ctx context.Context, frame Frame) (Frame, error) {
	switch req := frame.(type) {
	case ToolListRequest:
		return ToolListResponse{
			ID:    req.ID,
			Type:  TypeToolsListOK,
			Tools: s.tools,
		}, nil
	case PingRequest:
		return PongResponse{
			ID:     req.ID,
			Type:   TypePong,
			Status: s.status,
		}, nil
	case ToolCallRequest:
		handler, ok := s.handlers[req.Tool]
		if !ok {
			return ErrorResponse{
				ID:      req.ID,
				Type:    TypeError,
				Code:    "TOOL_NOT_FOUND",
				Message: fmt.Sprintf("tool %q is not registered", req.Tool),
			}, nil
		}
		result, err := handler(ctx, req.Params)
		if err != nil {
			msg := err.Error()
			return ToolResultResponse{
				ID:     req.ID,
				Type:   TypeToolResult,
				Result: nil,
				Error:  &msg,
			}, nil
		}
		return ToolResultResponse{
			ID:     req.ID,
			Type:   TypeToolResult,
			Result: &result,
			Error:  nil,
		}, nil
	default:
		return nil, fmt.Errorf("unsupported request frame %T", frame)
	}
}
