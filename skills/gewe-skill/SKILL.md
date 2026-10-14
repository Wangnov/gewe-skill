---
name: gewe-skill
description: Use GeWe-backed WeChat memory for read-only Agent analysis, identity resolution, message window queries, Moments/SNS analysis, summaries, and chatroom event inspection through the gewe-skill CLI/API.
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

## 命令约定

macOS 上优先使用 `gewe`：它通过 SSH 连接记忆服务器，安全透传参数并在远端加载 token；下载和导出的输出文件保存在本机。先检查连接：

```bash
gewe doctor
```

消息阅读优先用 `--format text`。需要稳定 ID、message_key、候选、原始证据或自动处理时，省略该参数取得兼容 JSON；`--compact` 仅去掉消息中的 `raw_content` 与 `msg_source`。`gewe-skill` 本身默认也是 JSON，直接调用它时仍需配置 `GEWE_SKILL_BASE_URL`、读写 token。

包装命令源码在仓库 `scripts/gewe.sh`，安装位置为 `~/.local/bin/gewe`。支持 `GEWE_SSH_TARGET`、`GEWE_SSH_KEY`、`GEWE_REMOTE_ENV`、`GEWE_REMOTE_CLI` 配置；不要把 token 写入仓库或输出到对话。新消息功能需远端 CLI 与 memory 更新，定向补拉还需新版 edge；连接诊断可继续使用旧服务。

## 推荐阅读路径

用户给出人名、群名和时间范围时，先用一个组合查询：

```bash
gewe query messages --conversation '测试好友' --dm --after '2026-10-13T00:00:00+08:00' --limit 50 --format text
gewe query messages --conversation '测试群' --sender '测试成员' --limit 50 --format text
gewe query messages --conversation '测试群' --q '关键词' --limit 50 --format text
```

人名命中好友且有对应私聊时，默认优先私聊；群名精确命中仍选群。`--dm` 只解析已有私聊，无私聊时返回 `dm_conversation_unresolved`，不会转到该人所在的群。候选按实体合并，`sources` 汇集来源，`chatroom_ids` 保留群范围。未解析时用 JSON 检查候选和名称，不要自行扩大查询范围。

`--after` 是接收时间下界，包含边界；`--before` 不包含边界。默认 `--order desc` 返回范围内**最新 N 条**；需要从起点读起时使用 `--order asc`，返回范围内**最早 N 条**。文本最后一行说明选择规则、时间范围、更早/更晚是否有未返回消息、翻页参数和附件缺失条数；更早/更晚均指同一筛选范围内的消息，翻页须保留原会话、发送者、关键词和时间条件。

```bash
gewe query messages --conversation '测试好友' --dm --after '2026-10-13T00:00:00+08:00' --order asc --limit 50 --format text
gewe query messages --conversation-id 'wxid_test_friend' --after '2026-10-13T00:00:00+08:00' --order desc --cursor '<返回的 cursor>' --format text
```

时间默认使用 `Asia/Shanghai`，可用 `--timezone UTC` 或本机 `TZ` 覆盖。消息时间和筛选都以 `received_at` 为准，接收时间可能晚于微信发送时间。文本每条一行，使用有效显示名，自发消息显示“我”；图片、文件、引用、语音等显示简短标记，系统入群/改名消息显示人话。语音“未转写”不代表没有音频。

需要单条消息的稳定键或原始字段时，切到 JSON：

```bash
gewe query messages --conversation '测试好友' --dm --kind image --limit 5 --compact
gewe identity resolve --q '测试好友' --limit 10
gewe identity inspect --wxid 'wxid_test_friend' --chatroom-id 'test_group@chatroom'
```

JSON 的 `messages.pagination.selection` 为 `latest`、`earliest`，上下文为 `context`；`has_more` 与 `next_cursor` 描述当前方向是否还有下一页。服务端返回的 cursor 为不透明字符串，原样透传，可区分同一时间戳的消息；最后一页 `next_cursor=null`。`attachments` 和 `voice.items` 精确对应返回的消息键；无需先扫描最近附件。

姓名使用 `effective_display_name`：联系人备注优先，其次群名片、群昵称，再到联系人昵称、别名。跨群结果优先使用 `speakers.by_conversation[conversation_id][sender_id]`；历史 v2 私聊未填 `sender_wxid` 时显示查询使用 `from_user` 回退。确实需要当前名片或成员状态时，再按 `memory_status` 对该群/联系人做有限刷新，不要为普通读消息固定先跑维护。

```bash
gewe identity warm --chatroom-id 'test_group@chatroom' --recent-messages 200 --max-contacts 50
gewe identity refresh --wxids 'wxid_test_friend'
gewe messages list --conversation-id 'wxid_test_friend' --limit 50 --format text
gewe messages search --conversation-id 'wxid_test_friend' --q '关键词' --limit 20 --format text
gewe messages context --message-key '<message_key>' --before 5 --after 5 --format text
```

JSON 的 `agent_guidance.next_actions` 可用于有限翻页、身份刷新和语音检查。优先保持已解析的精确 ID；附件缺失时使用下文的按消息补拉，不能据缺失记录断言上游媒体已丢失。

## 朋友圈分析

Use this path when the user asks to analyze a friend's 朋友圈/Moments. The user usually provides a fuzzy nickname, WeChat ID, alias, or at most a plain wxid; do not ask them for GeWe `v3` identifiers. Start with `--contact` and let the memory service resolve the contact through GeWe:

```bash
gewe-skill --json sns fetch --contact '<nickname-or-wechat-id>' --limit 50
```

This call resolves the contact, pages through `contactsSnsList`, stores every scanned post in `gewe-skill-memory`, and returns the bounded post window. It only captures posts that GeWe exposes in `snsList`: if WeChat shows pinned/profile-only Moments but `contactsSnsList` returns an empty first page, report that source limitation and do not infer that the contact has no Moments. GeWe `snsDetails` requires a known `snsId`, so it cannot recover pinned items that never appear in the list response. If the user gave an exact internal wxid/v3, use the escape hatch:

```bash
gewe-skill --json sns fetch --target-wxid '<wxid_or_v3>' --limit 50
```

Use time windows when the user asks for them:

```bash
gewe-skill --json sns fetch --contact '<nickname-or-wechat-id>' --after '<iso-time>' --before '<iso-time>' --limit 100 --max-pages 100
```

For analysis, export a local package for the analysis step:

```bash
gewe sns export --contact '<nickname-or-wechat-id>' --limit 50 --output /tmp/gewe-sns-analysis.md --format markdown
```

`sns export` refreshes and stores posts by default before writing the local file. Use `--no-refresh --target-wxid '<wxid>'` only when you intentionally want to export already stored posts.

Markdown exports include post ids, timestamps, text, content style, media counts, and safe derived fields, but do not expand media URLs. Use JSONL only when the user explicitly needs raw records:

```bash
gewe sns export --contact '<nickname-or-wechat-id>' --limit 50 --output /tmp/gewe-sns-analysis.jsonl --format jsonl
```

When analyzing, distinguish stored facts from inference. Likes and comments are only the visible data for the current account's social graph, so do not treat missing interaction records as proof of low popularity. Prefer content themes, posting cadence, time window, media mix, and recurring topics.

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
gewe-skill --json query chatroom-events --conversation '<chatroom wording>' --limit 100
gewe-skill --json query chatroom-events --conversation '<chatroom wording>' --event-type member_joined,member_left,member_removed --limit 100
gewe-skill --json query chatroom-events --conversation '<chatroom wording>' --after '<iso-time>' --before '<iso-time>' --limit 100
gewe-skill --json chatrooms events --chatroom-id '<chatroom_id>' --limit 100
gewe-skill --json chatrooms system-events --chatroom-id '<chatroom_id>' --limit 100
gewe-skill --json chatrooms snapshots --chatroom-id '<chatroom_id>' --limit 20
```

Prefer `query chatroom-events` for Agent answers because it resolves the group name, enriches wxids with current identity display memory, and merges member/system events into one timeline. Keep wxid fields as stable evidence, but use `*_display_name` fields when explaining people to the user. Prefer structured system events for actor/target names. Prefer snapshot diff events for actual membership state changes. If they disagree, report the disagreement.

## 附件

用户要求查看实际媒体时，直接按消息下载，不需要先查询 sha256：

```bash
gewe attachments download --message-key '<message_key>' -o ./附件.jpg
gewe attachments download --message-key '<message_key>' -o - > ./附件.jpg
```

`-o -` 或省略输出路径时，stdout 只有附件字节，不夹带 JSON。指定本机文件时，包装命令先写临时文件，下载失败不覆盖已有成品。下载会校验 SHA-256；同一消息多个附件时返回选择列表，使用其中的 `--index N`（从 1 开始）：

```bash
gewe attachments list --message-key '<message_key>'
gewe attachments download --message-key '<message_key>' --index 1 -o ./附件.jpg
```

尚无附件记录且用户需要该媒体时，定向补拉这一条并等待最多 180 秒：

```bash
gewe sync attachment-repair --message-key '<message_key>' --wait-seconds 180
gewe attachments download --message-key '<message_key>' -o ./附件.jpg
```

该命令仅补建/重排该消息的下载任务，按消息同步完成文件，不读全局统计、不改全局同步游标。保护未过期的 processing 租约；`unavailable`、`purged`、超限和非文件等终态不会反复重置，需按返回状态说明限制。超时会明确返回 `status=timeout`，edge 任务仍可继续；稍后用相同消息键重试，不需要跑全局 repair。定向补拉必须在新版 edge 上使用。

附件记录与下载响应按文件头识别 MIME，魔数优先于错误的上游值。检测到附件、已同步字节、已下载到本机、已转写分别说明。多条消息共享相同 sha256 是正常的内容去重，不是缺失。

MIME 回填属于维护操作，用户要求时才运行；先预览再应用：

```bash
gewe maintenance attachment-mime-backfill --dry-run
gewe maintenance attachment-mime-backfill --apply
```

## Voice and transcripts

Voice messages have two stages: the callback message may exist before the audio file is available. Always check voice availability before relying on voice content:

```bash
gewe-skill --json voice list --conversation-id '<conversation_id>' --limit 20
gewe-skill --json voice list --conversation-id '<conversation_id>' --missing-only --limit 20
```

Interpret `availability` like this:

- `missing_attachment`: the voice message exists, but the audio bytes are not in the attachment library yet.
- `ready`: the audio bytes are available and can be transcribed.
- `transcribed`: a transcript is already stored.
- `failed`: transcription was attempted and failed; inspect `error`.

Transcribe only bounded windows. This may call a paid or quota-limited ASR provider:

```bash
gewe-skill --json voice transcribe --message-key '<message_key>' --provider codex-asr --language zh
gewe-skill --json voice warm --conversation-id '<conversation_id>' --limit 10 --provider codex-asr --language zh
```

Supported providers are `codex-asr` and `cloudflare`. Prefer `codex-asr` when available because it is self-hosted and better suited for higher-volume private WeChat voice messages. Use Cloudflare only for small bounded batches unless the user approves the quota/cost tradeoff.

The memory service may auto-transcribe voice attachments when they are synced. Do not assume every ready voice already has a transcript: if the ASR service was temporarily unavailable, run a bounded warm pass before analysis:

```bash
gewe-skill --json voice warm --conversation-id '<conversation_id>' --limit 20 --provider codex-asr
```

`voice warm` is idempotent. It keeps completed transcripts, retries failed transcripts, and skips messages that still lack attachments.

Voice transcription is content-deduplicated. If two voice messages point to the same downloaded attachment `sha256`, the memory service reuses the existing successful transcript for the same provider and language instead of calling ASR again. Message-level transcript records are still written so each chat message remains independently explainable.

## Trusted maintenance path

Start with a read-only status check before trusted ingest, sync, or repair workflows:

```bash
gewe-skill --json maintenance status
```

Before broad media, file, or voice analysis, prefer the Agent-oriented readiness report:

```bash
gewe-skill --json maintenance data-health --with-edge-queue
```

If it reports `stale_active_attachment_count` greater than zero, run the recommended bounded `sync attachment-requeue` before concluding that the queue is merely waiting. A healthy queue may have short-lived active jobs, but old pending, processing, or overdue retry-scheduled jobs are treated as stale maintenance items.

Use the status output to decide whether the problem is missing callbacks, missing synced attachments, missing voice transcripts, stale identity memory, or chatroom event coverage.

Before name-sensitive analysis over recent speakers, use the bounded identity health report. It reads local message/profile memory only and does not poll the full contact list:

```bash
gewe-skill --json maintenance identity-health --conversation-id '<chatroom_id>' --limit 100
```

If it returns `overall_health=needs_identity_warm`, run the recommended `identity warm` command for the affected chatroom before confidently explaining current remarks, group cards, nicknames, joins/leaves, or who said what. This keeps refresh scoped to the chatroom and recent active speakers.

If chatroom event answers still show raw wxids instead of names, run a bounded identity backfill over recent chatroom events:

```bash
gewe-skill --json maintenance identity-backfill --dry-run --event-limit 500
gewe-skill --json maintenance identity-backfill --event-limit 500 --max-chatrooms 10 --max-wxids 100
```

This first refreshes only chatrooms seen in recent events, then refreshes only still-missing wxids. Do not use broad full-contact refresh unless the user explicitly approves the larger GeWe request surface.

If `voice.ready_without_completed_transcript` is greater than zero, run a bounded ASR backfill:

```bash
gewe-skill --json maintenance voice-issues --with-edge-queue --limit 50
gewe-skill --json maintenance attachment-queue-health --limit 1000
gewe-skill --json sync attachment-queue --asset-type voice --status failed --limit 20
gewe-skill --json sync attachment-repair --backfill-limit 100 --sync-limit 100
gewe-skill --json maintenance voice-repair --limit 20 --provider codex-asr --language zh
gewe-skill --json maintenance asr-backfill --limit 20 --provider codex-asr --language zh
```

Prefer `maintenance voice-issues` before repair. It returns an Agent-readable action list:

- `missing_attachment`: 优先用 `sync attachment-repair --message-key` 补拉该消息，再决定是否进行 ASR。
- `asr_pending`: run bounded `voice transcribe` or `maintenance asr-backfill`.
- `asr_failed`: retry only after the provider or decoder issue is fixed.
- `asr_failed` with `retryable=false`: treat it as a known transcript gap and explain it; do not retry it in a loop.

Prefer `maintenance voice-repair` when the user asks to repair voice coverage. It runs a bounded ASR warm pass and returns before/after issue counts. It does not sync missing attachments; if `missing_attachment` remains, run `sync attachment-repair --asset-type voice` first and then rerun `voice-repair`.

Prefer `maintenance attachment-queue-health` when the user asks whether attachments are healthy across images, voice, video, emoji, and files. It returns counts by status and asset type, duplicate queue evidence, job-key-precise completed-but-not-ingested memory evidence, and Agent-readable next actions.

Prefer `maintenance data-health --with-edge-queue` before broad analysis when the user cares about media, voice, freshness, or whether the archive is ready. It returns `overall_health`, `ready_for_analysis`, and ordered `next_actions` so attachment sync and queue safety happen before ASR.

单条消息的 `missing_attachment` 优先使用上述定向补拉。只有用户要求整体队列维护时，再使用以下全局队列命令。 For `missing_attachment`, inspect the edge-backed attachment queue before ASR. Use `sync attachment-queue` to inspect queue state, `sync attachment-backfill` to create missing download jobs from stored messages, `sync attachment-requeue` to re-send pending or stale retryable jobs, `sync attachment-retry` only for intentional terminal retries, and `sync attachment-repair` as the Agent-friendly bounded sweep that backfills, requeues, and syncs completed files into memory. `sync attachment-repair` defaults to `--asset-type all` for image, voice, video, emoji, and file jobs; narrow it with `--asset-type voice` only when you intentionally want a voice-only repair. Treat `sync attachment-repair` `after_queue_health` as the final repair outcome summary; if `completed_not_ingested_count` is greater than zero, run another bounded attachment repair before analyzing media.

When `attachment-queue-health` returns `retryable_terminal_jobs`, explain that these are failed jobs that may be retried deliberately after checking `last_error_summary`. When it returns `non_retryable_terminal_jobs`, explain unavailable/purged media as upstream or queue-retention evidence rather than as normal missing sync. Do not loop on `unavailable`, `purged`, or `skipped_not_file` unless the user explicitly asks for another upstream retry.

When `voice-issues --with-edge-queue` returns `edge_queue_evidence.status=unavailable` or `purged`, explain that the edge queue already proved the upstream attachment is not currently downloadable. Do not keep retrying unavailable media unless the user explicitly asks for another upstream retry.

The server can also run the same ASR backfill in the background when `GEWE_SKILL_ASR_BACKGROUND_ENABLED=true`. Keep the background limit small, prefer `codex-asr`, and keep `GEWE_SKILL_ASR_BACKGROUND_FORCE=false` so known decoder failures do not become a busy retry loop.

Use these only for trusted ingest, sync, or repair workflows:

```bash
gewe-skill --json ingest normalize --file callback.json --received-at 2026-05-26T00:00:00.000Z
gewe-skill --json ingest file --file callback.json --received-at 2026-05-26T00:00:00.000Z
gewe-skill --json sync edge --limit 100
gewe-skill --json sync attachment-repair --sync-limit 50
gewe-skill --json sync chatroom-events --limit 500
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
- `query messages` returns compact `speakers.by_wxid` by default; use `effective_display_name` for human-facing explanations and keep `sender_wxid` as stable evidence.
- Use `identity inspect` when explaining why an old alias maps to a current person, especially when `identity resolve` selected a non-current alias.
- Observed aliases from quoted messages are useful evidence, but may be historical. Current GeWe group member info has higher confidence for present state.
- For group-card or nickname changes, preserve the original message text and avoid over-normalizing.
- For files, images, voice, video, and emoji, mention whether the attachment was downloaded or only detected.

## Not yet supported

The architecture reserves room for future WeChat write APIs, but this skill currently must not call any send-message or mutation endpoint.
