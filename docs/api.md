# API

推荐 Agent 用 `gewe query messages ... --format text`，HTTP API 是 gewe-skill-memory 的底层协议；包装命令通过 SSH 加载远端认证，token 不复制到本机。

## 认证与路由

Bearer 读 token 保护消息、身份、附件、语音查询；identity refresh 只更新本地缓存，不写微信。write 路由、SNS fetch 和 ASR 操作需要写 token。GET /healthz 无需认证。

```text
POST /write/events
POST /write/raw-events
POST /write/attachments
POST /write/chatroom-events
POST /write/maintenance/attachment-mime
POST /api/sns/fetch
POST /api/voice/transcribe
POST /api/voice/warm
GET  /api/messages
GET  /api/messages/recent
GET  /api/messages/search
GET  /api/messages/{message_key}/context
GET  /api/conversations
GET  /api/identity/resolve
GET  /api/identity/profile
POST /api/identity/refresh
GET  /api/attachments/recent
POST /api/attachments/by-message-keys
POST /api/attachments/by-job-keys
GET  /api/attachments/{sha256}/download
GET  /api/voice
POST /api/voice/by-message-keys
GET  /api/sns/posts
GET  /api/maintenance/status
GET  /api/chatrooms/{chatroom_id}/snapshots
GET  /api/chatrooms/{chatroom_id}/events
GET  /api/chatrooms/{chatroom_id}/system-events
```

## 消息窗口

GET /api/messages 支持群聊和私聊。

| 参数 | 含义 |
| --- | --- |
| conversation_id | 精确群 ID 或私聊 wxid。 |
| sender_wxid | 精确发送者。 |
| q | 文本或消息 JSON/XML 子串。 |
| kind | text、image、quote、voice、file 等，不区分大小写。 |
| direction | incoming、outgoing；省略则两者。 |
| dm | true 时只查私聊。 |
| after / before | 带时区 RFC3339 接收时间，包含 after、不含 before，按实际时刻比较。 |
| cursor | 返回的 cursor 原样透传；旧 RFC3339 时间游标仍接受。 |
| limit | 默认 50，限制 1–200。 |
| order | 默认 desc 返回最新 N 条，asc 返回最早 N 条。 |

**带 after 仍默认选范围内最新 N 条**，默认排序不变。时间、筛选和分页基于 received_at，不使用 wechat_created_at；回调延迟可使二者不同。

JSON 保留 items 与 next_cursor，增加 pagination：order、selection（latest/earliest）、time_basis（received_at）、returned、start_at/end_at、has_more、has_earlier/has_later、earlier_cursor/later_cursor。最后一页 next_cursor=null。has_more 是当前方向是否有下一页；双向标志是同一筛选范围内、当前页以外是否还有消息，不表示整个会话的历史完整性。更早用 order=desc 加 earlier_cursor，更晚用 order=asc 加 later_cursor，保留原筛选。游标由接收时间和行 ID 区分相同时间戳的消息，调用方应视为不透明字符串。

上下文保留 before、anchor、after、message_key，增加 pagination，selection=context；找不到 anchor 仍返回空上下文。

```bash
gewe query messages --conversation '测试好友' --dm --after '2026-10-13T00:00:00+08:00' --format text
gewe messages list --conversation-id 'wxid_test_friend' --order asc --format text
gewe messages search --q '关键词' --conversation-id 'test_group@chatroom' --format text
gewe messages context --message-key '<message_key>' --before 5 --after 5 --format text
```

query messages、messages list/search/context 支持 format=text，每条一行，使用有效显示名和简短非文本标记，群系统 XML 显示人话。默认时区 Asia/Shanghai，timezone 优先于 TZ。末行汇总范围、排序、双向翻页和附件缺失；缺失指应有附件但无存储记录的消息，关闭查询时显示未检查。默认 JSON 不删除字段，compact 仅移除消息 raw_content、msg_source。

## 身份解析

GET /api/identity/resolve?q=...&limit=10 搜索当前/历史群名、联系人备注/昵称/别名、群名片、群昵称和引用别名。候选按实体合并，同一人或群只出现一次；source 保留，新增 sources、chatroom_ids、matched_aliases 数组。

CLI 会话解析先选群名精确命中；其余情况下，命中好友且存在对应私聊时优先私聊。dm 只接受已有私聊；未命中返回 ok=false、executed=false、error=dm_conversation_unresolved 和可读 hint。

GET /api/identity/profile?wxid=...&chatroom_id=... 返回 effective_display_name、来源和新鲜度。优先联系人备注、群名片、群昵称、联系人昵称、别名。组合查询保留 speakers.by_wxid，增加 speakers.by_conversation 适配跨群名片；历史 v2 私聊缺少 sender_wxid 时显示查询用 from_user 回退，原消息 JSON 不改写。

## 附件与语音

POST /api/attachments/by-message-keys 请求 `{"message_keys":["<message_key>"]}`，返回对应消息全部附件；by-job-keys 同理使用 job_keys。POST /api/voice/by-message-keys 使用同一形状，只返回这些消息中的语音和转写，避免读取无关窗口。每次最多处理 1000 个消息键。

下载底层仍为 GET /api/attachments/{sha256}/download。CLI 做精确消息键查询，一条消息多附件时必须 index=N（从 1 开始）选择；attachments list --message-key 不再过滤全局最近清单。

```bash
gewe attachments download --message-key '<message_key>' -o ./附件.jpg
gewe attachments download --message-key '<message_key>' --index 1 -o - > ./附件.jpg
gewe sync attachment-repair --message-key '<message_key>' --wait-seconds 180
```

stdout 下载不夹 JSON，校验 SHA-256；包装命令将指定文件写到本机，失败不覆盖已有文件。直接 CLI 普通文件保存仍返回 JSON 报告。

定向 repair 调用新版 edge 的 POST /admin/repair-attachment?message_key=...，轮询按消息过滤的 /admin/attachments、/admin/download-jobs，并下载写回 memory。只补建/重排目标消息任务，保护活跃 processing 租约、已完成任务和不可下载终态；不调用全局 stats/backfill/requeue、不改全局同步游标。默认总等待 180 秒，允许 1–600 秒；超时 ok=false、status=timeout，edge 任务继续。CLI 与 memory 必须共享 attachment_dir。

## MIME 与回填

memory 入库、附件查询和下载响应按头部识别 JPEG/PNG/GIF/WebP/MP4/SILK/AMR/PDF/ZIP。魔数覆盖缺失、通用、拼写错误或矛盾的上游 MIME；未知魔数保留有效值，octst-stream 规范为 octet-stream。ZIP 容器不细分办公文档。edge 新下载也检查文件头，重放已读块后继续流式存 R2。

POST /write/maintenance/attachment-mime 受写 token 保护，请求 `{"dry_run":true,"after_id":0,"limit":200}`，默认 dry_run=true。按 ID 分批读前 64 字节，报告 scanned、changed、missing_files、changes、next_after_id；应用时事务内同时更新 MIME 列和 attachment_json。缺失文件跳过计数。若同步任务并发更新了记录，回填跳过并计入 concurrent_updates，不覆盖新元数据；可再次运行。CLI 自动遍历所有批次：

```bash
gewe maintenance attachment-mime-backfill --dry-run
gewe maintenance attachment-mime-backfill --apply
```

读取修正不写数据库，启动不自动回填。定向补拉依赖新版 edge；OCI 脚本不部署 Worker、不自动回填。

## 朋友圈 / SNS

POST /api/sns/fetch 根据 contact 模糊解析，或使用 target_wxid，分页读 GeWe contactsSnsList 后保存；支持 after/before、limit、max_pages、decrypt。GET /api/sns/posts 查询已存内容，支持 target_wxid、q、after/before、cursor、limit、order。

仅 contactsSnsList 返回的朋友圈可以入库。微信主页可见的置顶项如果接口未返回，会报告来源限制；不能据空结果断言没有朋友圈。snsDetails 需要已知 snsId，不能恢复从未出现在列表中的内容。

```bash
gewe sns fetch --contact '测试好友' --limit 50
gewe sns export --contact '测试好友' --limit 50 -o ./朋友圈.md --format markdown
gewe sns export --no-refresh --target-wxid 'wxid_test_friend' -o - --format jsonl
```

sns export 默认刷新再导出，no-refresh 只导出已存记录；`-o -` 为纯文档 stdout，gewe 将指定输出路径落到本机。微信发消息等写操作不属于本次能力。
