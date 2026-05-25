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
