# gewe-skill CLI

`gewe-skill-memory` 的 Agent CLI，默认兼容 JSON。macOS 推荐用本机 `gewe`，通过 SSH 加载远端 token，下载和导出写入本机。

```bash
install -m 0755 scripts/gewe.sh ~/.local/bin/gewe
gewe doctor
gewe query messages --conversation '测试好友' --dm --after '2026-10-13T00:00:00+08:00' --limit 50 --format text
gewe query messages --conversation '测试群' --sender '测试成员' --limit 50 --format text
```

`~/.local/bin` 需要在 PATH 中，安装不修改 shell 配置。包装命令只依赖 Bash/OpenSSH，支持 GEWE_SSH_TARGET、GEWE_SSH_KEY、GEWE_REMOTE_ENV、GEWE_REMOTE_CLI。连不上会提示检查 Tailscale，GEWE_SSH_VERBOSE=1 可显示原始诊断。新消息能力需远端 CLI 和 memory 同时更新。

## 消息与姓名

人名命中好友且存在私聊时优先私聊；群名精确命中仍选群。`--dm` 只解析已有私聊，也可不带名称查询所有私聊；无私聊返回 dm_conversation_unresolved，不退到群。候选按实体去重，sources 合并来源，chatroom_ids 保留群范围。

`--after` 包含下界，`--before` 不含上界，均按接收时间 received_at。默认 `--order desc` 返回范围内**最新 N 条**，`--order asc` 返回**最早 N 条**；加 after 不改变规则。服务端 limit 为 1–200。

```bash
gewe query messages --conversation '测试好友' --dm --order asc --format text
gewe messages list --conversation-id 'wxid_test_friend' --format text
gewe messages search --q '关键词' --conversation-id 'wxid_test_friend' --format text
gewe messages context --message-key '<message_key>' --before 5 --after 5 --format text
gewe query messages --conversation '测试好友' --dm --kind image --limit 5 --compact
```

文本每条一行，使用 effective_display_name 和简短非文本标记；自发消息显示“我”，群系统 XML 显示人话。时间默认 Asia/Shanghai，支持 `--timezone UTC` 或 TZ。末行给出条数、接收时间范围、选择规则、更早/更晚是否有未返回消息、翻页参数和附件缺失条数。翻页须保留原筛选条件并原样透传 cursor，可区分同一时间戳多条消息。

JSON 原字段保留，新增 pagination，selection 为 latest/earliest/context，has_more 说明当前方向是否有下一页；最后一页 next_cursor=null。`--compact` 只移除消息的 raw_content 和 msg_source；默认不裁剪。语音“未转写”不等于音频缺失。附件缺失计数指应有附件但无记录的消息；关闭附件查询时显示未检查。

## 附件与定向补拉

```bash
gewe attachments download --message-key '<message_key>' -o ./附件.jpg
gewe attachments download --message-key '<message_key>' -o - > ./附件.jpg
gewe attachments list --message-key '<message_key>'
gewe attachments download --message-key '<message_key>' --index 1 -o ./附件.jpg
gewe sync attachment-repair --message-key '<message_key>' --wait-seconds 180
```

省略输出路径也输出二进制 stdout。多个附件时列出候选，须指定 `--index N`（从 1 开始）。旧的 `--sha256` 下载保留。下载验证 SHA-256；stdout 不夹 JSON。直接 CLI 保存普通文件仍返回 JSON 报告，包装命令的保存提示写 stderr，失败不覆盖已有成品。

定向 repair 只补建/重排该消息的任务，按消息同步，不改全局游标；默认总等待 180 秒，可设 1–600 秒。保护活跃 processing 租约，不重置 unavailable/purged/超限等终态。超时返回 status=timeout，edge 任务继续，已完成附件保留；稍后同键重试。需要新版 edge，不带消息键的旧全局 repair 保持原行为。

包装命令只重定向附件下载和 sns export 的输出。同步命令 attachment-dir、维护游标和 ingest 输入文件仍是服务器路径，不能指定本机输入文件。

## MIME 与朋友圈导出

JPEG、PNG、GIF、WebP、MP4、SILK、AMR、PDF、ZIP 按文件头识别，魔数覆盖缺失、通用、拼写错误及矛盾的上游 MIME。已有记录的查询/下载响应会修正显示；永久回填同时更新 MIME 列和 JSON，需要写 token：

```bash
gewe maintenance attachment-mime-backfill --dry-run
gewe maintenance attachment-mime-backfill --apply
gewe sns export --contact '测试好友' --limit 50 -o ./朋友圈.md --format markdown
gewe sns export --no-refresh --target-wxid 'wxid_test_friend' -o - --format jsonl
```

回填默认 dry-run，启动时不自动应用，缺失文件会计数。无法识别时保留有效上游 MIME，octst-stream 规范为 octet-stream；ZIP 不进一步区分 DOCX/XLSX。sns export 默认经 GeWe 刷新并存储，再写文件；no-refresh 只导出已有记录。contactsSnsList 未返回的置顶/主页朋友圈会报告来源限制，不能据空结果断言不存在。

验证和上线说明见根目录 DELIVERY-2026-10-15.md、docs/testing.md；OCI 上线脚本不部署 Worker，也不自动运行 MIME 回填。
