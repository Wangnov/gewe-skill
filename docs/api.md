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
GET /api/identity/resolve
POST /api/identity/refresh
GET /api/attachments/recent
GET /api/attachments/{sha256}/download
GET /api/chatrooms/{chatroom_id}/snapshots
GET /api/chatrooms/{chatroom_id}/events
GET /api/chatrooms/{chatroom_id}/system-events
```

Future WeChat mutation APIs are intentionally out of scope for the current version.

## Identity memory

`GET /api/identity/resolve?q=...` resolves human names into stable ids before an Agent reads messages. It searches current and historical group names, contact nicknames, contact remarks, aliases, and room-scoped member display names.

`POST /api/identity/refresh` asks the memory service to call GeWe read-only APIs and update the local identity index. This mutates only the Agent-readable memory cache, not WeChat itself.

```bash
gewe-skill resolve --q 'DuckCoding技术喝水交流群'
gewe-skill refresh-identity --recent-chatrooms 20
gewe-skill refresh-identity --chatroom-id '12345@chatroom'
```

## Attachment flow

`GET /api/attachments/recent` returns synced attachment metadata, including `kind`, `message_key`, `sha256`, `size_bytes`, and `mime_type`.

Use `GET /api/attachments/{sha256}/download` or the CLI wrapper below when actual bytes are needed:

```bash
gewe-skill attachment-download --sha256 '<sha256>' --output /tmp/gewe-attachment.bin
```
