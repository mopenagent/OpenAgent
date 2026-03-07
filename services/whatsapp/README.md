# WhatsApp Service

Go service that connects OpenAgent to WhatsApp via [whatsmeow](https://github.com/tulir/whatsmeow). Runs as an MCP-lite daemon managed by `ServiceManager`; generates QR codes for linking, receives messages, and sends replies.

## Overview

- **Runtime:** Go (whatsmeow)
- **Protocol:** MCP-lite over Unix socket (`data/sockets/whatsapp.sock`)
- **Events:** `whatsapp.qr`, `whatsapp.message.received`, `whatsapp.connection.status`
- **Tools:** `whatsapp.status`, `whatsapp.link_state`, `whatsapp.send_text`

## Linking Your Phone

1. Ensure WhatsApp is configured in `config/openagent.yaml`:
   ```yaml
   platforms:
     whatsapp:
       data_dir: data   # whatsapp.db stored at data/whatsapp.db
   ```

2. Build the service: `make whatsapp`

3. Enable WhatsApp in Settings > Connector

4. Open Settings > Connector, click **Scan QR code** on the WhatsApp card

5. Scan the QR code with WhatsApp on your phone: **Settings → Linked Devices → Link a Device**

## Configuration

| Method | Variable | Example |
|--------|----------|---------|
| Config | `platforms.whatsapp.data_dir` | `data` |
| Env | `WHATSAPP_DATA_DIR` | `data` |

Session data (device store) is stored at `data_dir/whatsapp.db` (e.g. `data/whatsapp.db`).

## Build

```bash
make whatsapp
# or
cd services/whatsapp && go build -o bin/whatsapp .
```

## Migration from session.db

If you previously used `data/whatsapp/whatsapp.db`, copy it:
- `cp data/whatsapp/whatsapp.db data/whatsapp.db`
- Then remove the empty dir: `rmdir data/whatsapp`

## Troubleshooting

| Issue | Fix |
|-------|-----|
| "QR timed out" | QR codes expire. Click Scan QR code again for a fresh one. |
| No QR shown | Ensure the service is running (Services page). Enable WhatsApp in Connector. |
| "invalid chat_id" | Use full JID: `15551234567@s.whatsapp.net` |
