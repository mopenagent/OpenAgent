# OpenAgent MCP-lite Go SDK

Shared protocol package for OpenAgent services, aligned with Python pydantic models in:

- `openagent/services/protocol.py`

## Package

- `mcplite` - frame types, newline JSON codec, and request dispatcher helpers.

## Install (from service module)

```go
require github.com/kmaneesh/openagent/services/sdk-go v0.0.0
```

Use a local `replace` while developing in this monorepo:

```go
replace github.com/kmaneesh/openagent/services/sdk-go => ../../services/sdk-go
```

## Usage

```go
decoder := mcplite.NewDecoder(conn)
encoder := mcplite.NewEncoder(conn)

server := mcplite.NewServer(toolDefs, "ready")
server.RegisterToolHandler("echo", func(ctx context.Context, params map[string]any) (string, error) {
    text, _ := params["text"].(string)
    return text, nil
})

for {
    frame, err := decoder.Next()
    if err != nil {
        return err
    }
    response, err := server.HandleRequest(context.Background(), frame)
    if err != nil {
        return err
    }
    if err := encoder.WriteFrame(response); err != nil {
        return err
    }
}
```
