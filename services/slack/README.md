# Slack Service

Rust service that connects OpenAgent to Slack via [slack-morphism-rust](https://github.com/abdolence/slack-morphism-rust). Runs as an MCP-lite daemon managed by `ServiceManager`; receives messages via Socket Mode and can send replies using `slack.send_message`.

## Overview

- **Runtime:** Rust (slack-morphism)
- **Protocol:** MCP-lite over Unix socket (`data/sockets/slack.sock`)
- **Events:** `slack.message.received`, `slack.connection.status`
- **Tools:** `slack.status`, `slack.link_state`, `slack.send_message`

The service uses **Socket Mode** for real-time events (no public URL required). You need both a **bot token** (xoxb-...) and an **app-level token** (xapp-...) with `connections:write`.

---

## Connecting to Slack

### Step 1: Create a Slack App

1. Go to [api.slack.com/apps](https://api.slack.com/apps)
2. Click **Create New App** → **From scratch**
3. Enter an **App Name** (e.g. `OpenAgent`) and select your **Workspace**
4. Click **Create App**

### Step 2: Configure Bot Token Scopes

1. In the left sidebar, open **OAuth & Permissions**
2. Under **Bot Token Scopes**, click **Add an OAuth Scope**
3. Add at least:
   - `chat:write` — Send messages
   - `channels:history` — Read public channel messages (if needed)
   - `groups:history` — Read private channel messages (if needed)
   - `im:history` — Read DM messages
   - `mpim:history` — Read group DM messages

### Step 3: Enable Socket Mode

1. In the left sidebar, open **Socket Mode**
2. Toggle **Enable Socket Mode** to **On**
3. If prompted, create an **App-Level Token**:
   - Click **Generate**
   - Name it (e.g. `OpenAgent`)
   - Add scope: `connections:write`
   - Click **Generate**
4. Copy the **App-Level Token** — it starts with `xapp-`

### Step 4: Create an App-Level Token (if not done in Step 3)

1. Open **Basic Information** in the left sidebar
2. Scroll to **App-Level Tokens**
3. Click **Generate Token and Scopes**
4. Name: e.g. `OpenAgent`
5. Add scope: `connections:write`
6. Copy the token (xapp-...)

### Step 5: Install the App to Your Workspace

1. Open **OAuth & Permissions**
2. Click **Install to Workspace**
3. Review permissions and click **Allow**
4. Copy the **Bot User OAuth Token** — it starts with `xoxb-`

### Step 6: Configure OpenAgent

Set credentials via env vars or config:

| Method | Variables | Example |
|--------|-----------|---------|
| Env vars | `SLACK_BOT_TOKEN`, `SLACK_APP_TOKEN` | See below |
| Config | `config/openagent.yaml` → `platforms.slack` | See below |

**Environment variables:**

```bash
export SLACK_BOT_TOKEN="<your-bot-token>"      # xoxb-...
export SLACK_APP_TOKEN="<your-app-token>"      # xapp-...
```

**config/openagent.yaml:**

```yaml
platforms:
  slack:
    bot_token: "<your-bot-token>"   # xoxb-...
    app_token: "<your-app-token>"   # xapp-...
```

- **SLACK_BOT_TOKEN** (xoxb-...) — Required. Used for API calls and Socket Mode auth.
- **SLACK_APP_TOKEN** (xapp-...) — Required for receiving messages. Without it, the service starts but won't get events.

Env vars override config. Prefer env vars for production.

### Step 7: Invite the Bot to Channels

1. In Slack, open a channel where you want the bot
2. Type `/invite @YourBotName` or use **Add apps** in the channel menu
3. The bot must be in the channel to read and reply to messages

---

## Communicating with Channels

### Receiving Messages

Incoming messages are emitted as `slack.message.received` events:

```json
{
  "event": "slack.message.received",
  "data": {
    "channel_id": "C01234ABCDE",
    "user_id": "U01234ABCDE",
    "text": "Hello bot!",
    "ts": "1234567890.123456",
    "team_id": "T01234ABCDE"
  }
}
```

Use `channel_id` when replying.

### Sending a Reply

Use the `slack.send_message` tool:

```json
{
  "channel_id": "C01234ABCDE",
  "text": "Hello from OpenAgent!"
}
```

The agent loop uses `platform.send_message` with `platform: "slack"`. Replies go to the channel from the incoming event.

### Getting Channel IDs

Slack channel IDs start with `C` (channels), `G` (private channels), or `D` (DMs). To find a channel ID:

1. Right-click the channel in Slack
2. Click **View channel details** (or **Copy link**)
3. The ID is in the URL: `.../archives/C01234ABCDE`

Or use the Slack API: `conversations.list` returns channel IDs.

---

## Tools Reference

| Tool | Description |
|------|-------------|
| `slack.status` | Returns `running`, `connected`, `authorized`, `backend`, `bot_user_id`, `team_id`, optional `last_error` |
| `slack.link_state` | Returns `authorized`, `connected`, `backend`, `bot_user_id`, `team_id` |
| `slack.send_message` | Send text to a channel. Params: `channel_id`, `text` |

---

## Build

```bash
# From repo root
make slack
# or (current host only)
make local
# or
cd services/slack && cargo build --release && cp target/release/slack ../../bin/slack-darwin-arm64
```

---

## Troubleshooting

| Issue | Fix |
|-------|-----|
| "missing SLACK_BOT_TOKEN" | Set bot token via env or config |
| Bot doesn't receive messages | Enable Socket Mode and set SLACK_APP_TOKEN with `connections:write` |
| "not_in_channel" when sending | Invite the bot to the channel with `/invite @YourBotName` |
| "missing_scope" | Add required OAuth scopes in OAuth & Permissions |
| Can't find channel ID | Right-click channel → View channel details; ID is in the URL |
