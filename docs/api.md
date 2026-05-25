# API

`gewe-skill-memory` exposes two API groups.

## Trusted write API

```text
POST /write/events
POST /write/raw-events
POST /write/attachments
```

This endpoint is for `gewe-skill-edge` or trusted repair tools only.

## Read-only Agent API

```text
GET /api/messages/recent
GET /api/messages/search
GET /api/conversations
GET /api/attachments/recent
GET /api/attachments/{sha256}/download
GET /api/chatrooms/{chatroom_id}/snapshots
GET /api/chatrooms/{chatroom_id}/events
GET /api/chatrooms/{chatroom_id}/system-events
```

Future WeChat mutation APIs are intentionally out of scope for the current version.

## Attachment flow

`GET /api/attachments/recent` returns synced attachment metadata, including `kind`, `message_key`, `sha256`, `size_bytes`, and `mime_type`.

Use `GET /api/attachments/{sha256}/download` or the CLI wrapper below when actual bytes are needed:

```bash
gewe-skill attachment-download --sha256 '<sha256>' --output /tmp/gewe-attachment.bin
```
