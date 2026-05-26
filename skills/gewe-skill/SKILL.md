---
name: gewe-skill
description: Use GeWe-backed WeChat memory for read-only Agent analysis, identity resolution, message window queries, summaries, and chatroom event inspection through the gewe-skill CLI/API.
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

## Command contract

This CLI is Agent-first. Always use `--json`; stdout is the machine-readable result and stderr is for diagnostics.

Start with:

```bash
gewe-skill --json doctor
```

The CLI reads these environment variables:

```bash
GEWE_SKILL_BASE_URL=http://127.0.0.1:8788
GEWE_SKILL_READ_TOKEN=...
GEWE_SKILL_WRITE_TOKEN=...
```

## Normal read path

1. Resolve human wording before reading messages:

```bash
gewe-skill --json identity resolve --q '<chatroom/contact/member wording>' --limit 10
```

2. Before serious analysis of a named group, warm that one chatroom. This refreshes the chatroom and only recent active speakers, instead of polling the whole contact list:

```bash
gewe-skill --json identity warm --chatroom-id '<chatroom_id>' --recent-messages 200 --max-contacts 50
```

3. If names still look stale or missing, refresh only the needed identity scope:

```bash
gewe-skill --json identity refresh --chatroom-id '<chatroom_id>'
gewe-skill --json identity refresh --wxids '<wxid1>,<wxid2>'
gewe-skill --json identity refresh --recent-chatrooms 20
```

4. Read a bounded message window. Use the resolved `conversation_id` for both group chats and private chats:

```bash
gewe-skill --json messages list --conversation-id '<conversation_id>' --limit 50
gewe-skill --json messages list --conversation-id '<conversation_id>' --after '<iso-time>' --before '<iso-time>' --limit 50
gewe-skill --json messages list --conversation-id '<conversation_id>' --sender-wxid '<wxid>' --limit 50
gewe-skill --json messages list --conversation-id '<conversation_id>' --kind text --direction incoming --limit 50
```

5. Search within a scoped window instead of broad global search when the user named a group/person/time:

```bash
gewe-skill --json messages search --q '<keyword>' --conversation-id '<conversation_id>' --limit 20
gewe-skill --json messages search --q '<keyword>' --conversation-id '<conversation_id>' --after '<iso-time>' --limit 20
```

6. If a specific message matters, fetch nearby context:

```bash
gewe-skill --json messages context --message-key '<message_key>' --before 5 --after 5
```

## Conversation discovery

Use this when the user names a vague group or asks what data exists:

```bash
gewe-skill --json conversations list --limit 100
gewe-skill --json identity resolve --q '<name from user>' --limit 10
```

Prefer `identity resolve` over keyword search for names. If multiple candidates remain, report the candidates and do not guess.

## Chatroom member changes

Use both event surfaces before claiming who joined, left, was removed, or renamed something:

```bash
gewe-skill --json chatrooms events --chatroom-id '<chatroom_id>' --limit 100
gewe-skill --json chatrooms system-events --chatroom-id '<chatroom_id>' --limit 100
gewe-skill --json chatrooms snapshots --chatroom-id '<chatroom_id>' --limit 20
```

Prefer structured system events for actor/target names. Prefer snapshot diff events for actual membership state changes. If they disagree, report the disagreement.

## Attachments

List attachment metadata first. Download bytes only when the user asks to inspect actual media or files:

```bash
gewe-skill --json attachments list --limit 20
gewe-skill --json attachments list --message-key '<message_key>' --limit 50
gewe-skill --json attachments download --sha256 '<sha256>' --output /tmp/gewe-attachment.bin
```

Mention whether media was downloaded or only detected.

## Trusted maintenance path

Use these only for trusted ingest, sync, or repair workflows:

```bash
gewe-skill --json ingest normalize --file callback.json --received-at 2026-05-26T00:00:00.000Z
gewe-skill --json ingest file --file callback.json --received-at 2026-05-26T00:00:00.000Z
gewe-skill --json sync edge --limit 100
gewe-skill --json sync attachments --limit 50
```

## Raw escape hatch

Use high-level commands first. If a read-only endpoint is not exposed yet, use:

```bash
gewe-skill --json request get --path /api/messages --query conversation_id=<conversation_id> --query limit=20
```

Do not use raw writes unless the user asked for that specific write.

## Interpretation rules

- Resolve names first, then read messages by stable ids.
- Treat room-scoped member aliases as scoped to `chatroom_id`; the same display name may appear in multiple groups.
- For chatroom members, prefer the user's contact remark when available, then room-scoped display/card names, then nicknames.
- Observed aliases from quoted messages are useful evidence, but may be historical. Current GeWe group member info has higher confidence for present state.
- For group-card or nickname changes, preserve the original message text and avoid over-normalizing.
- For files, images, voice, video, and emoji, mention whether the attachment was downloaded or only detected.

## Not yet supported

The architecture reserves room for future WeChat write APIs, but this skill currently must not call any send-message or mutation endpoint.
