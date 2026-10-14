# gewe-skill 易用性改进交付（2026-10-15）

五项能力均已在工作区实现，基于 HEAD `140e5f3`，未 commit、未 push。CLI/memory 包版本保留 0.1.48，实际差异由本次工作区源码和构建产物体现，交给 Claude 审查后决定版本与提交。

本机 `~/.local/bin/gewe` 已安装，权限 755，与 `scripts/gewe.sh` 一致；当前 PATH 已包含 `~/.local/bin`，没有修改 shell 配置。生产仍运行原 0.1.48：本次仅只读 SSH 验证，没有安装/替换服务器程序、重启服务、修改 systemd、回填数据库或部署 Cloudflare。私人文件未触碰；新增例子与测试身份均为虚构值，生产 token、密钥内容、真实 wxid/群 ID 没有写入文件。

## 1. 本机包装命令

选择方案 **(a)：Bash 包装 SSH 到远端 CLI**。本机只依赖 macOS 自带 Bash/OpenSSH，无需安装本地 Rust CLI、维护常驻隧道或复制 token。CLI 与服务器版本一致；参数用 POSIX 引号编码逐个透传，支持中文、空格、单/双引号、换行与 shell 特殊字符。已用模拟 SSH 验证参数不会被当作代码执行。

默认带 `--json`，`--format text` 时不补该标志；调用方显式 `--json` 不会重复添加。远端使用现有 root 配置加载认证，不输出凭据。连接失败会提示检查 Tailscale、密钥与目标；`GEWE_SSH_VERBOSE=1` 可查看原始 SSH 诊断。

```bash
gewe doctor
gewe query messages --conversation '测试好友' --after '2026-10-13T00:00:00+08:00' --limit 50 --format text
gewe attachments download --message-key '<message_key>' -o ./附件.jpg
gewe sns export --no-refresh --target-wxid 'wxid_test_friend' -o ./朋友圈.md --format markdown
```

连接/路径可通过 `GEWE_SSH_TARGET`、`GEWE_SSH_KEY`、`GEWE_REMOTE_ENV`、`GEWE_REMOTE_CLI` 配置。下载与 SNS 导出把远端 `-o` 转为 stdout，再写本机；先写目标目录的临时文件，成功才替换成品，失败保留原文件。保存提示走 stderr。

**升级前的实际状态：** `gewe doctor` 已对现有服务器成功执行，health 和认证只读探测均通过。新消息参数需要远端 CLI/memory 更新。包装命令会先检查 stdout 能力，旧 CLI 的文件命令会被明确拒绝，避免把 `-` 当作服务器文件名。

## 2. 可读文本与分页

`query messages`、`messages list/search/context` 均支持 `--format text`，每条一行：`MM-DD HH:MM 显示名: 正文`。使用已有 effective_display_name 顺序，跨群保留各自名片，自发消息显示“我”；历史 v2 私聊缺少 sender_wxid 时显示查询回退到 from_user，原消息 JSON 不改写。

图片显示 SHA-256 前 8 位，文件显示标题/文件名，语音显示已完成转写或“未转写”，引用显示被引用摘要与正文，链接/表情/视频显示简短标记；群系统模板渲染成一句人话。时间默认 Asia/Shanghai，可用 `--timezone` 或本机 `TZ` 覆盖。展示、过滤与分页统一基于**接收时间 received_at**。

```bash
gewe query messages --conversation '测试好友' --dm --after '2026-10-13T00:00:00+08:00' --order asc --format text
gewe messages search --conversation-id 'wxid_test_friend' --q '关键词' --format text --timezone UTC
gewe messages context --message-key '<message_key>' --before 5 --after 5 --format text
gewe query messages --conversation '测试好友' --dm --kind image --limit 5 --compact
```

**排序结论：** 原默认 desc 保持，带 after 仍取范围内最新 N 条；asc 取最早 N 条。末行给出返回条数、范围、选择规则、更早/更晚未返回消息、翻页参数与附件缺失数。双向标志只针对同一筛选范围，翻页保留会话、发送者、关键词和时间条件。

JSON 保留既有字段，新增 pagination 的 selection/order/time_basis、范围和双向翻页信息；`--compact` 仅移除消息中的 raw_content、msg_source。修正原先“任何非空页都有 next_cursor”的误报：耗尽时现在为 null。新 cursor 以时间与行 ID 区分同时间戳消息，调用方原样透传；旧时间 cursor 仍接受。

为确保语音与消息窗口一致，增加按 message_keys 查询转写的只读 API；组合查询不再读取该范围内不相关的语音。时间条件按实际时刻比较，覆盖带 +08:00 的边界；新增 SQLite 时间表达式索引支持查询。

## 3. 按姓名优先私聊

命中好友且有对应私聊时，默认选择该私聊；群名精确命中仍选择群。`--dm` 强制只在已有私聊中解析，也可不带名称只查私聊。没有私聊返回 `dm_conversation_unresolved` 与清晰 hint，不扩大到群。

候选按实体合并，同一人或群只出现一次；保留 source，新增 sources 来源数组、chatroom_ids 群范围数组与 matched_aliases 别名数组，去重发生在结果限额之前。群内发送者解析仍保留对应范围。

测试覆盖“好友在多个群仍选私聊”“精确群名优先”“无私聊 unresolved”，以及合并来源、群范围与 room-scoped 显示名。`query chatroom-events` 保留原先面向群的解析路径。

## 4. 按消息下载与定向补拉

```bash
gewe attachments download --message-key '<message_key>' -o ./附件.jpg
gewe attachments download --message-key '<message_key>' -o - > ./附件.jpg
gewe attachments list --message-key '<message_key>'
gewe attachments download --message-key '<message_key>' --index 1 -o ./附件.jpg
gewe sync attachment-repair --message-key '<message_key>' --wait-seconds 180
```

省略 -o 也输出纯二进制 stdout。下载验证 SHA-256，保留旧 --sha256 方式；一条消息多个附件时列出候选并要求 **--index N（从 1 开始）**，不自动挑选。attachments list --message-key 使用精确查询，不再从最近清单过滤。

定向 repair 只为该消息补建/重排任务，再按该消息同步完成字节；不调用全局 stats/backfill/requeue，不改全局同步游标。保护未过期 processing 租约、已完成任务；重排 pending/retry_scheduled/failed 或过期 processing。unavailable/purged/超限/非文件等终态保留解释，不循环重置。默认总等待 180 秒，允许 1–600 秒；超时明确返回 status=timeout，edge 任务继续，稍后同键重试。

edge 新增 `/admin/repair-attachment`，附件清单和下载任务清单支持 message_key 过滤。CLI 会拒绝未按目标消息过滤的响应，避免同步无关附件。旧的无消息键全局 repair 不变。

**依赖：定向 repair 需要新版 Worker。** 本次 edge 只有代码与本地测试，未部署；OCI 上线脚本也不包含 Worker 部署。真实生产图片没有在线补拉；完整成功/超时、字节回写和游标保护通过本机临时 memory + 模拟 edge 验证。

## 5. MIME 识别与回填

memory 的新附件入库、存量附件查询和下载响应按头部识别 JPEG/PNG/GIF/WebP/常见 MP4/SILK/AMR/PDF/ZIP，识别结果优先于缺失、通用、octst-stream 错字或矛盾的上游值。edge 新下载同样读取前缀并重放到流，测试确认分块保存不会丢字节。

已有记录可以永久回填；默认 dry-run，需写 token：

```bash
gewe maintenance attachment-mime-backfill --dry-run
gewe maintenance attachment-mime-backfill --apply
```

按 ID 分批读前 64 字节，在事务中同时更新 MIME 列与 attachment_json。缺失文件报告 missing_files；无法识别时保留有效上游值，错误通用值规范为 octet-stream。并发同步已改变的记录不会被覆盖，跳过并计入 concurrent_updates，必要时再次运行。启动与只读查询不自动执行永久回填。

## 文档与验证结果

已更新仓库 `skills/gewe-skill/SKILL.md`、CLI README、docs/api.md，推荐 Agent 先用 gewe + text、私聊模式、按消息下载与定向补拉；同步更新 testing/deployment 文档和 CI 的本机端到端/edge 测试步骤。

| 检查 | 结果 |
| --- | --- |
| cargo test --workspace --locked / --all-targets | 62 项 Rust 测试通过；文档测试无失败。 |
| cargo clippy --workspace --all-targets --locked | 退出 0，遵循现有 pedantic=warn 标准；有现有文件告警，新增模块无告警，不声称 -D warnings 通过。 |
| cargo fmt --all -- --check | 通过。 |
| Python 本机端到端脚本 | 15 项通过，临时 SQLite、虚构身份、模拟 SSH/edge，全程无生产写入。 |
| edge npm test / npm run check | 7 项测试和 TypeScript 检查通过。 |
| bash -n scripts/*.sh / git diff --check | 通过。 |
| skill-creator quick_validate | 通过。 |
| 本机安装与生产只读验证 | 包装命令文件一致、PATH 可发现；gewe doctor 健康/读认证通过。 |
| 服务器只读环境检查 | aarch64、glibc 2.39、原服务 active、CLI 0.1.48、配置 root/600，edge admin token 仅确认存在。 |

重跑本机验证：

```bash
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked
cargo build --locked -p gewe-skill-cli -p gewe-skill-memory --bins
python3 scripts/test-usability-2026-10-15.py
bash -n scripts/*.sh
cd apps/gewe-skill-edge
npm test
npm run check
```

edge 测试需 Node 22.18+，本机实际使用 Node 26.6.0；最低版本依据见 [Node TypeScript 文档](https://nodejs.org/api/typescript.html#modules-typescript)。

## ARM64 产物与待放行上线脚本

**已在本机实际交叉编译** 两份 aarch64-unknown-linux-gnu 产物，file 确认为 ARM aarch64 ELF 64-bit，动态解释器 `/lib/ld-linux-aarch64.so.1`。产物位于：

```text
target/aarch64-unknown-linux-gnu/release/gewe-skill
target/aarch64-unknown-linux-gnu/release/gewe-skill-memory
```

本次最终产物 SHA-256（后续源码或工具链变动后应重建）：

| 产物 | SHA-256 |
| --- | --- |
| gewe-skill | 5e1c930166da95cade318d5517053cf3c405dcc1e0fa979ca27b83a09814bf54 |
| gewe-skill-memory | cdea645bcc893f4bf165e7411c4ac9cb11c8f287ff908b4348acf3bbcb81212a |

本机工具链：Rust/Cargo 1.96.0、已安装 aarch64-unknown-linux-gnu target、cargo-zigbuild 0.20.1、Zig 0.16.0。构建使用 GNU glibc 下限 2.28；SQLite C 代码也由 Zig 交叉构建，无需在 OCI 装编译器。对应命令：

```bash
cargo zigbuild --locked --release --target aarch64-unknown-linux-gnu.2.28 \
  -p gewe-skill-cli -p gewe-skill-memory --bin gewe-skill --bin gewe-skill-memory
```

尚未准备工具链的机器需要 Rust stable、上述 Rust target、cargo-zigbuild、Zig，以及 SSH/scp、file、shasum、rg。当前两份 Linux 产物没有在生产执行过，本机功能验证使用 macOS 原生构建与临时服务。

`scripts/deploy-oci-2026-10-15.sh` 已写好并通过语法检查，**本次没有运行它的任何模式**。负责人放行后的入口：

```bash
scripts/deploy-oci-2026-10-15.sh --deploy
# 不重新构建时：使用已审查的当前产物
GEWE_DEPLOY_PREBUILT=1 scripts/deploy-oci-2026-10-15.sh --deploy
# MIME 是独立一步，先看预览再选择 apply
scripts/deploy-oci-2026-10-15.sh --mime-dry-run
scripts/deploy-oci-2026-10-15.sh --mime-apply
```

普通 deploy 在本机编译或选用明确的预编译产物，上传后校验 SHA-256，保存 `/opt/gewe-skill-memory/bin/backups/<UTC时间戳>-<进程号>/` 的旧二进制和校验清单，再原子替换两份程序；只重启 gewe-skill-memory.service。验收同时检查服务 active、healthz 与认证只读消息查询，失败自动恢复备份、重启并复查。备份保留；不改 env、systemd、定时器或 Cloudflare。MIME apply 会先执行 dry-run，不由普通 deploy 自动触发。

回滚逻辑已做代码/语法审查，未在生产演练。断电、SIGKILL 或无法连接服务器时不能保证自动回滚完成；脚本保留备份位置，并在无法确认恢复时明确报告。启动新增查询索引会保留在数据库中，旧程序可兼容；二进制回滚不删除它们。

## 已知限制

- 新消息能力依赖远端 CLI/memory 更新，定向补拉另依赖 edge 更新；当前生产保持原版本，新功能验收来自本机测试数据。
- 旧时间游标无法区分同一时间戳多条消息；新返回游标已修正。末行的双向“还有消息”仅针对同一筛选范围，不代表微信历史完整。
- 附件缺失统计指存储记录缺失，不是逐条下载文件的实测；语音未转写与音频缺失分别处理。多附件需显式 index。
- 本机包装仅重定向附件下载与 SNS 导出。ingest 的输入文件、同步 attachment-dir 和维护游标仍是服务器路径；不是本机文件上传工具。
- MIME 只识别列出的常见头部；ZIP 不细分 Office 文档，未把 HEIC/AVIF 等 ftyp 容器误判为 MP4。文件缺失无法回填。
- SNS 默认 export 会调用 GeWe 刷新；已有数据导出使用 no-refresh。全局维护命令依然存在，单条附件优先用定向 repair。
