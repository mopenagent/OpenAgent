# Discord Service

Go service that connects OpenAgent to Discord via the [discordgo](https://github.com/bwmarrin/discordgo) library. Runs as an MCP-lite daemon managed by `ServiceManager`; receives messages from Discord channels and DMs, and can send/edit messages via tools.

## Overview

- **Runtime:** Go (discordgo)
- **Protocol:** MCP-lite over Unix socket (`data/sockets/discord.sock`)
- **Events:** `discord.message.received`, `discord.connection.status`
- **Tools:** `discord.status`, `discord.link_state`, `discord.send_message`, `discord.edit_message`

The service subscribes to guild messages and direct messages. Incoming messages are emitted as events; the agent loop can reply via `discord.send_message` or `discord.edit_message`.

---

## Registering a Bot for Interaction

### 1. Create an Application

1. Go to the [Discord Developer Portal](https://discord.com/developers/applications)
2. Click **New Application**
3. Enter a name (e.g. "OpenAgent Bot") and click **Create**

### 2. Create a Bot User

1. In the left sidebar, open **Bot**
2. Click **Add Bot**
3. (Optional) Set a username and avatar

### 3. Configure Bot Settings

Under **Bot**:

- **Privileged Gateway Intents** — enable:
  - **Message Content Intent** — required to read message text
  - **Server Members Intent** — optional, for member info
- **Public Bot** — toggle off if you want invite-only

### 4. Get the Bot Token

1. Under **Bot**, click **Reset Token** (or **View Token** if already set)
2. Copy the token — it looks like `MTQ3ODc5Njk4MzY5NDI2NjQ3MQ.GfMo9f.xxxx`
3. **Never commit this token.** Store it in env vars or `config/openagent.yaml` (gitignored)

### 5. Invite the Bot to Your Server

1. Open **OAuth2** → **URL Generator**
2. **Scopes:** select `bot`
3. **Bot Permissions:** select at least:
   - **Send Messages**
   - **Read Message History**
   - **View Channels**
   - **Read Messages/View Channels**
4. Copy the generated URL and open it in a browser
5. Choose your server and authorize

---

## Configuration

Set the bot token via one of:

| Method | Variable | Example |
|--------|----------|---------|
| Env var | `DISCORD_BOT_TOKEN` or `OPENAGENT_DISCORD_BOT_TOKEN` | `export DISCORD_BOT_TOKEN="MTQ3..."` |
| Config | `config/openagent.yaml` → `platforms.discord.token` | See below |

**config/openagent.yaml:**

```yaml
platforms:
  discord:
    token: "YOUR_BOT_TOKEN"
    guild_ids: []   # Optional: limit to specific server IDs
```

Env vars override config. Prefer env vars for production.

---

## Communicating with a Channel

### Getting Channel IDs

Discord uses numeric IDs (snowflakes) for channels. To get a channel ID:

1. **Enable Developer Mode** in Discord:
   - User Settings → App Settings → Advanced → Developer Mode: **On**
2. **Right-click** the channel (or DM) in the sidebar
3. Click **Copy Channel ID**

Channel IDs look like `1234567890123456789`.

### Sending a Message to a Channel

Use the `discord.send_message` tool:

```json
{
  "channel_id": "1234567890123456789",
  "text": "Hello from OpenAgent!"
}
```

**Via platform.send_message (agent tool):**

```json
{
  "platform": "discord",
  "channel_id": "1234567890123456789",
  "text": "Hello!"
}
```

The agent loop routes `platform.send_message` to `discord.send_message` when `platform` is `"discord"`.

### Editing a Message

Use `discord.edit_message`:

```json
{
  "channel_id": "1234567890123456789",
  "message_id": "9876543210987654321",
  "text": "Updated message text"
}
```

Get `message_id` from the `discord.message.received` event (`id` field) or from the result of `discord.send_message`.

### Receiving Messages

Incoming messages are emitted as `discord.message.received` events:

```json
{
  "event": "discord.message.received",
  "data": {
    "id": "msg-snowflake",
    "channel_id": "channel-snowflake",
    "guild_id": "guild-snowflake",
    "author_id": "user-snowflake",
    "author": "Username",
    "content": "Hello bot!",
    "is_bot": false
  }
}
```

The `PlatformManager` subscribes to these events and routes them into the message bus. Replies use `channel_id` from the event.

---

## Tools Reference

| Tool | Description |
|------|-------------|
| `discord.status` | Returns `running`, `connected`, `authorized`, `backend`, optional `last_error` |
| `discord.link_state` | Returns `authorized`, `connected`, `backend` |
| `discord.send_message` | Send text to a channel. Params: `channel_id`, `text` |
| `discord.edit_message` | Edit an existing message. Params: `channel_id`, `message_id`, `text` |

---

## Build

```bash
# From repo root
make discord
# or
cd services/discord && go build -o bin/discord .
```

---

## Troubleshooting

| Issue | Fix |
|-------|-----|
| "missing DISCORD_BOT_TOKEN" | Set token via env or config |
| Bot doesn't receive messages | Enable **Message Content Intent** in Developer Portal → Bot |
| "Missing Access" when sending | Ensure bot has **Send Messages** and **View Channel** permissions |
| Can't get channel ID | Enable Developer Mode in Discord settings |
