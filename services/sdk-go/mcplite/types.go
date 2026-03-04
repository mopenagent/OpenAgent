package mcplite

// Stable MCP-lite frame type values.
const (
	TypeToolsList   = "tools.list"
	TypeToolCall    = "tool.call"
	TypePing        = "ping"
	TypeToolsListOK = "tools.list.ok"
	TypeToolResult  = "tool.result"
	TypePong        = "pong"
	TypeError       = "error"
	TypeEvent       = "event"
)

// Frame is the shared protocol abstraction for encode/decode operations.
type Frame interface {
	FrameType() string
}

// ToolDefinition matches Python pydantic ToolDefinition.
type ToolDefinition struct {
	Name        string         `json:"name"`
	Description string         `json:"description"`
	Params      map[string]any `json:"params"`
}

// ToolListRequest is sent by the agent.
type ToolListRequest struct {
	ID   string `json:"id"`
	Type string `json:"type"`
}

func (f ToolListRequest) FrameType() string { return f.Type }

// ToolCallRequest is sent by the agent.
type ToolCallRequest struct {
	ID     string         `json:"id"`
	Type   string         `json:"type"`
	Tool   string         `json:"tool"`
	Params map[string]any `json:"params"`
}

func (f ToolCallRequest) FrameType() string { return f.Type }

// PingRequest is sent by the agent.
type PingRequest struct {
	ID   string `json:"id"`
	Type string `json:"type"`
}

func (f PingRequest) FrameType() string { return f.Type }

// ToolListResponse is sent by the service.
type ToolListResponse struct {
	ID    string           `json:"id"`
	Type  string           `json:"type"`
	Tools []ToolDefinition `json:"tools"`
}

func (f ToolListResponse) FrameType() string { return f.Type }

// ToolResultResponse is sent by the service.
type ToolResultResponse struct {
	ID     string  `json:"id"`
	Type   string  `json:"type"`
	Result *string `json:"result"`
	Error  *string `json:"error"`
}

func (f ToolResultResponse) FrameType() string { return f.Type }

// PongResponse is sent by the service.
type PongResponse struct {
	ID     string `json:"id"`
	Type   string `json:"type"`
	Status string `json:"status"`
}

func (f PongResponse) FrameType() string { return f.Type }

// ErrorResponse is sent by the service.
type ErrorResponse struct {
	ID      string `json:"id"`
	Type    string `json:"type"`
	Code    string `json:"code"`
	Message string `json:"message"`
}

func (f ErrorResponse) FrameType() string { return f.Type }

// EventFrame is sent by the service without request correlation id.
type EventFrame struct {
	Type  string         `json:"type"`
	Event string         `json:"event"`
	Data  map[string]any `json:"data"`
}

func (f EventFrame) FrameType() string { return f.Type }
