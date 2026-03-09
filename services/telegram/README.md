# Telegram Service

Rust service that connects OpenAgent to Telegram via [teloxide](https://github.com/teloxide/teloxide) (Bot API). Runs as an MCP-lite daemon managed by `ServiceManager`; receives private messages and can send replies using `telegram.send_message`.

## Overview

- **Runtime:** Rust (teloxide)
- **Protocol:** MCP-lite over Unix socket (`data/sockets/telegram.sock`)
- **Events:** `telegram.message.received`, `telegram.connection.status`
- **Tools:** `telegram.status`, `telegram.link_state`, `telegram.send_message`

The service runs in **bot mode** and handles private (DM) messages only. Incoming messages are emitted as events; the agent replies via `telegram.send_message` using `from_id` (chat_id) from the event. The Bot API does not use `access_hash`; it is emitted as `0` for compatibility.

---

## Connecting a Telegram Bot

You need only a **bot token** from [@BotFather](https://t.me/BotFather).

### Step 1: Create a Bot and Get the Token

1. Open Telegram and search for **@BotFather**
2. Send `/newbot`
3. Enter a **display name** (e.g. `OpenAgent Assistant`)
4. Enter a **username** ending in `bot` (e.g. `openagent_assistant_bot`)
5. BotFather will reply with a token like `7123456789:AAHxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx`
6. Copy the token — **never share it or commit it to version control**

### Step 2: Configure OpenAgent

Set the token via env vars or config:

| Method | Variables | Example |
|--------|-----------|---------|
| Env vars | `TELEGRAM_BOT_TOKEN` | See below |
| Config | `config/openagent.yaml` → `platforms.telegram` | See below |

**Environment variables:**

```bash
export TELEGRAM_BOT_TOKEN="7123456789:AAHxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
```

**config/openagent.yaml:**

```yaml
platforms:
  telegram:
    bot_token: "7123456789:AAHxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
```

Env vars override config. Prefer env vars for production.

**Note:** `app_id` and `app_hash` (from my.telegram.org) are **not** required for the Bot API. They are ignored if present in config.

### Step 3: Start the Service

The `ServiceManager` starts the Telegram service automatically when OpenAgent runs. Ensure the binary is built:

```bash
make telegram
# or
make local
```

---

## Communicating with Users

### Receiving Messages

Incoming private messages are emitted as `telegram.message.received` events:

```json
{
  "event": "telegram.message.received",
  "data": {
    "message_id": 123,
    "from_id": 987654321,
    "access_hash": 0,
    "from_name": "John Doe",
    "username": "johndoe",
    "text": "Hello bot!"
  }
}
```

Use `from_id` when replying (Bot API uses it as chat_id for private chats).

### Sending a Reply

Use the `telegram.send_message` tool:

```json
{
  "user_id": 987654321,
  "text": "Hello from OpenAgent!"
}
```

`access_hash` is optional and ignored (Bot API).

### Finding Your User ID

To test or link identities, you need your Telegram user ID:

1. Message [@userinfobot](https://t.me/userinfobot) in Telegram
2. It will reply with your numeric user ID

---

## Tools Reference

| Tool | Description |
|------|-------------|
| `telegram.status` | Returns `running`, `connected`, `authorized`, `backend`, optional `last_error` |
| `telegram.link_state` | Returns `authorized`, `connected`, `backend` |
| `telegram.send_message` | Send text to a user. Params: `user_id`, `text` (access_hash ignored) |

---

## Build

```bash
# From repo root
make telegram
# or
make local
```

---

## Troubleshooting

| Issue | Fix |
|-------|-----|
| "missing TELEGRAM_BOT_TOKEN" | Set the token via env or config |
| Bot doesn't respond | Ensure you're sending a **private** message to the bot (DMs only) |
| "telegram runtime is not connected" | Check `telegram.status`; verify token and network |
