---
name: gewe-skill
description: Use GeWe-backed WeChat memory for read-only Agent analysis, search, summaries, and chatroom event inspection through the gewe-skill CLI/API.
---

# gewe-skill

Use this skill when the user wants an Agent to inspect, search, summarize, or analyze WeChat context collected through GeWe callbacks.

## Safety model

- Default to read-only operations.
- Do not send WeChat messages.
- Do not delete, modify, or rewrite source chat data unless the user gives explicit approval for that specific operation.
- Treat GeWe, WeChat local databases, and raw callback archives as source evidence.
- Treat `gewe-skill-memory` as the Agent-readable memory layer.
- Never print bearer tokens, GeWe tokens, callback secrets, or raw private payloads unless the user explicitly asks for a narrow diagnostic excerpt.

## Preferred access path

Use the CLI first:

```bash
gewe-skill health
gewe-skill recent --limit 20
gewe-skill search --q '<keyword>' --limit 20
gewe-skill conversations --limit 50
gewe-skill chatroom-events --chatroom-id '<chatroom_id>' --limit 50
gewe-skill chatroom-system-events --chatroom-id '<chatroom_id>' --limit 50
```

The CLI reads these environment variables:

```bash
GEWE_SKILL_BASE_URL=http://127.0.0.1:8788
GEWE_SKILL_READ_TOKEN=...
GEWE_SKILL_WRITE_TOKEN=...
```

Use write operations only for trusted ingest or repair workflows:

```bash
gewe-skill normalize --file callback.json --received-at 2026-05-26T00:00:00.000Z
gewe-skill ingest-file --file callback.json --received-at 2026-05-26T00:00:00.000Z
```

## Common tasks

### Recent chat context

1. Run `gewe-skill recent --limit 50`.
2. Identify relevant `conversation_id`, sender, time, and message previews.
3. Summarize with concrete timestamps and caveat if attachments are not loaded.

### Conversation inventory

1. Run `gewe-skill conversations --limit 100`.
2. Use the returned `conversation_id` values for follow-up message/event queries.

### Keyword search

1. Run `gewe-skill search --q '<keyword>' --limit 50`.
2. Use exact timestamps, `conversation_id`, `sender_wxid`, and message text in the answer.
3. If the result set is sparse, say that this is a keyword search over normalized text and may not include attachment-only content.

### Chatroom member changes

1. Run `gewe-skill chatroom-events --chatroom-id '<chatroom_id>' --limit 100`.
2. Run `gewe-skill chatroom-system-events --chatroom-id '<chatroom_id>' --limit 100`.
3. Correlate snapshot diff events with SYSTEM XML events before making claims about who joined, left, was invited, or was removed.

## Interpretation rules

- Prefer structured system events for actor/target names.
- Prefer snapshot diff events for actual membership state changes.
- If system events and snapshot diffs disagree, report the disagreement instead of guessing.
- For group-card or nickname changes, preserve the original message text and avoid over-normalizing.
- For files, images, voice, video, and emoji, mention whether the attachment was downloaded or only detected.

## Not yet supported

The architecture reserves room for future WeChat write APIs, but this skill currently must not call any send-message or mutation endpoint.
