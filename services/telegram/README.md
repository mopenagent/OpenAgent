# Telegram Service

Go service that connects OpenAgent to Telegram via the [gotd/td](https://github.com/gotd/td) MTProto client. Runs as an MCP-lite daemon managed by `ServiceManager`; receives private messages and can send replies using `telegram.send_message`.

## Overview

- **Runtime:** Go (gotd/td)
- **Protocol:** MCP-lite over Unix socket (`data/sockets/telegram.sock`)
- **Events:** `telegram.message.received`, `telegram.connection.status`
- **Tools:** `telegram.status`, `telegram.link_state`, `telegram.send_message`

The service runs in **bot mode** and handles private (DM) messages only. Incoming messages are emitted as events; the agent replies via `telegram.send_message` using `from_id` and `access_hash` from the event.

---

## Connecting a Telegram Bot

You need two sets of credentials:

1. **API credentials** (app_id, app_hash) from [my.telegram.org](https://my.telegram.org)
2. **Bot token** from [@BotFather](https://t.me/BotFather)

### Step 1: Get API Credentials (app_id, app_hash)

1. Go to [my.telegram.org](https://my.telegram.org)
2. Log in with your phone number (you’ll receive a code in Telegram)
3. Open **API development tools**
4. Create a new application:
   - **App title:** e.g. `OpenAgent`
   - **Short name:** e.g. `openagent`
   - **Platform:** Other
5. Submit the form
6. Copy **App api_id** (integer) and **App api_hash** (string)

**Important:** Each phone number can have only one API application. Keep these credentials private.

### Step 2: Create a Bot and Get the Token

1. Open Telegram and search for **@BotFather**
2. Send `/newbot`
3. Enter a **display name** (e.g. `OpenAgent Assistant`)
4. Enter a **username** ending in `bot` (e.g. `openagent_assistant_bot`)
5. BotFather will reply with a token like `7123456789:AAHxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx`
6. Copy the token — **never share it or commit it to version control**

### Step 3: Configure OpenAgent

Set credentials via env vars or config:

| Method | Variables | Example |
|--------|-----------|---------|
| Env vars | `TELEGRAM_APP_ID`, `TELEGRAM_APP_HASH`, `TELEGRAM_BOT_TOKEN` | See below |
| Config | `config/openagent.yaml` → `platforms.telegram` | See below |

**Environment variables:**

```bash
export TELEGRAM_APP_ID=12345678
export TELEGRAM_APP_HASH="a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6"
export TELEGRAM_BOT_TOKEN="7123456789:AAHxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
```

**config/openagent.yaml:**

```yaml
platforms:
  telegram:
    app_id: 12345678
    app_hash: "a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6"
    bot_token: "7123456789:AAHxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
```

Env vars override config. Prefer env vars for production.

### Step 4: Start the Service

The `ServiceManager` starts the Telegram service automatically when OpenAgent runs. Ensure the binary is built:

```bash
make telegram
# or
cd services/telegram && go build -o bin/telegram .
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
    "access_hash": 1234567890123456789,
    "from_name": "John Doe",
    "username": "johndoe",
    "text": "Hello bot!"
  }
}
```

Use `from_id` and `access_hash` when replying.

### Sending a Reply

Use the `telegram.send_message` tool:

```json
{
  "user_id": 987654321,
  "access_hash": 1234567890123456789,
  "text": "Hello from OpenAgent!"
}
```

The agent loop uses `platform.send_message` with `platform: "telegram"`. The adapter propagates `user_id` and `access_hash` from the incoming message metadata, so replies go to the correct user.

### Finding Your User ID

To test or link identities, you need your Telegram user ID:

1. Message [@userinfobot](https://t.me/userinfobot) in Telegram
2. It will reply with your numeric user ID
3. The `access_hash` is provided in each `telegram.message.received` event when someone messages your bot

---

## Tools Reference

| Tool | Description |
|------|-------------|
| `telegram.status` | Returns `running`, `connected`, `authorized`, `backend`, optional `last_error` |
| `telegram.link_state` | Returns `authorized`, `connected`, `backend` |
| `telegram.send_message` | Send text to a user. Params: `user_id`, `access_hash`, `text` |

---

## Build

```bash
# From repo root
make telegram
# or
cd services/telegram && go build -o bin/telegram .
```

---

## Troubleshooting

| Issue | Fix |
|-------|-----|
| "missing TELEGRAM_APP_ID, TELEGRAM_APP_HASH, or TELEGRAM_BOT_TOKEN" | Set all three via env or config |
| "API_ID_PUBLISHED_FLOOD" | Use your own app_id from my.telegram.org; sample IDs are rate-limited |
| Bot doesn't respond | Ensure you're sending a **private** message to the bot (DMs only) |
| "telegram runtime is not connected" | Check `telegram.status`; verify credentials and network |
