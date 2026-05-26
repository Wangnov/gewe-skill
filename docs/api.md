# API

`gewe-skill-memory` exposes trusted write endpoints and read-only Agent endpoints. The CLI is the preferred Agent surface; the HTTP API is the stable backing protocol.

## Trusted write API

```text
POST /write/events
POST /write/raw-events
POST /write/attachments
```

These endpoints are for `gewe-skill-edge`, the local sync timer, and trusted repair tools only.

## Read-only Agent API

```text
GET /api/messages
GET /api/messages/{message_key}/context
GET /api/conversations
GET /api/identity/resolve
POST /api/identity/refresh
GET /api/attachments/recent
GET /api/attachments/{sha256}/download
GET /api/chatrooms/{chatroom_id}/snapshots
GET /api/chatrooms/{chatroom_id}/events
GET /api/chatrooms/{chatroom_id}/system-events
```

`GET /api/messages` is the main read path for group chats and private chats. Supported query fields:

| Field | Meaning |
| --- | --- |
| `conversation_id` | Exact group id or private chat wxid. |
| `sender_wxid` | Exact sender id. |
| `q` | Text/XML substring search. |
| `kind` | Message type such as `text`, `image`, `quote`, `voice`, or `file`. |
| `direction` | `incoming`, `outgoing`, or omitted for both. |
| `after` / `before` | ISO timestamp window. |
| `cursor` | Pagination cursor from `next_cursor`. |
| `limit` | Bounded page size. |
| `order` | `desc` by default, or `asc`. |

CLI examples:

```bash
gewe-skill --json messages list --conversation-id '49407209075@chatroom' --limit 50
gewe-skill --json messages list --conversation-id '49407209075@chatroom' --sender-wxid 'hhyimiis' --after '2026-05-26T00:00:00Z' --limit 20
gewe-skill --json messages search --q 'grill-me' --conversation-id '49407209075@chatroom' --limit 20
gewe-skill --json messages context --message-key '<message_key>' --before 5 --after 5
```

## Identity memory

`GET /api/identity/resolve?q=...` resolves human names into stable ids before an Agent reads messages. It searches current and historical group names, contact nicknames, contact remarks, aliases, room-scoped member names, and observed aliases from quoted messages.

`POST /api/identity/refresh` asks the memory service to call GeWe read-only APIs and update the local identity index. This mutates only the Agent-readable memory cache, not WeChat itself.

```bash
gewe-skill --json identity resolve --q 'DuckCoding技术喝水交流群'
gewe-skill --json identity resolve --q '视频怪物' --chatroom-id '49407209075@chatroom'
gewe-skill --json identity refresh --recent-chatrooms 20
gewe-skill --json identity refresh --chatroom-id '12345@chatroom'
```

## Attachment flow

`GET /api/attachments/recent` returns synced attachment metadata, including `kind`, `message_key`, `sha256`, `size_bytes`, and `mime_type`.

Use `GET /api/attachments/{sha256}/download` or the CLI wrapper below when actual bytes are needed:

```bash
gewe-skill --json attachments list --limit 20
gewe-skill --json attachments download --sha256 '<sha256>' --output /tmp/gewe-attachment.bin
```

Future WeChat mutation APIs are intentionally out of scope for the current version.
