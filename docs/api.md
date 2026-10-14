# API

`gewe-skill-memory` exposes trusted write endpoints and read-only Agent endpoints. The CLI is the preferred Agent surface; the HTTP API is the stable backing protocol.

## Trusted write API

```text
POST /write/events
POST /write/raw-events
POST /write/attachments
POST /api/sns/fetch
```

`/write/*` endpoints are for `gewe-skill-edge`, the local sync timer, and trusted repair tools only. `POST /api/sns/fetch` is also write-token protected because it calls GeWe read APIs and persists fetched Moments/SNS posts into local memory.

## Read-only Agent API

```text
GET /api/messages
GET /api/messages/{message_key}/context
GET /api/conversations
GET /api/identity/resolve
POST /api/identity/refresh
GET /api/attachments/recent
GET /api/attachments/{sha256}/download
GET /api/sns/posts
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

## Moments / SNS memory

`POST /api/sns/fetch` resolves a friend from `contact` or accepts an exact `target_wxid`, pages through GeWe `contactsSnsList`, and upserts each scanned post into `sns_posts`.

Only posts returned by GeWe `contactsSnsList` can be stored. If a WeChat profile shows pinned/profile-only Moments but GeWe returns an empty `snsList`, the response includes a warning and the feature must report the source limitation instead of treating it as proof that the contact has no visible Moments. GeWe `snsDetails` requires a known `snsId`, so it cannot recover a pinned item that never appears in the list response.

Request fields:

| Field | Meaning |
| --- | --- |
| `contact` | Fuzzy nickname, WeChat ID, alias, or other user wording. The service resolves it through GeWe `contacts/search`. |
| `target_wxid` | Exact internal wxid/v3 escape hatch. |
| `after` / `before` | Optional ISO timestamp window for returned posts. Scanned posts are still persisted. |
| `limit` | Returned post limit, default 50. |
| `max_pages` | Maximum GeWe pages to scan. Useful for older time windows. |
| `decrypt` | Whether to ask GeWe for decrypted SNS content, default true. |

`GET /api/sns/posts` reads stored posts. Supported query fields are `target_wxid`, `q`, `after`, `before`, `cursor`, `limit`, and `order`.

CLI examples:

```bash
gewe-skill --json sns fetch --contact 'echochen55' --limit 50
gewe-skill --json sns list --target-wxid '<wxid_or_v3>' --limit 50
gewe-skill --json sns export --contact 'echochen55' --limit 50 --output /tmp/gewe-sns-analysis.md --format markdown
```

## Attachment flow

`GET /api/attachments/recent` returns synced attachment metadata, including `kind`, `message_key`, `sha256`, `size_bytes`, and `mime_type`.

Use `GET /api/attachments/{sha256}/download` or the CLI wrapper below when actual bytes are needed:

```bash
gewe-skill --json attachments list --limit 20
gewe-skill --json attachments download --sha256 '<sha256>' --output /tmp/gewe-attachment.bin
```

Future WeChat mutation APIs are intentionally out of scope for the current version.
