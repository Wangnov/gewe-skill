# macOS 用户级 memory 与 radar 部署

适用 Apple Silicon。无需 sudo；默认安装固定版本 v0.1.48 的 cli 与 memory，校验 release 包 SHA-256 后安装，并仅对已校验二进制去除 quarantine。既有 env 不会被覆盖。

```bash
GEWE_SKILL_VERSION=v0.1.48 scripts/install-macos-memory-release.sh
```

默认布局：`~/srv/gewe-skill-memory/{bin,config,launchd}`，`~/srv/wechat-radar/{app,config}`，`/Volumes/DATA/gewe.noindex/gewe-skill-memory/data` 与 `wechat-radar/{state,work}`，日志在 `~/Library/Logs/gewe-skill`。目录权限 0700，env 权限 0600。数据目录、env、radar 源码需事先准备；安装器不会创建空数据库或覆盖 env。

env 每行使用 `KEY='value'`；单引号需 shell 安全转义。不要打印、提交或记录任何凭据。memory 必须仅监听 `127.0.0.1:8788`，数据库 URL 使用 `?mode=rw`。以下五个变量都要指向外置盘上的绝对路径：

| 变量 | 文件 |
| --- | --- |
| GEWE_SKILL_SYNC_CURSOR_FILE | edge-sync.cursor |
| GEWE_SKILL_ATTACHMENT_CURSOR_FILE | edge-attachment-sync.cursor |
| GEWE_SKILL_CHATROOM_SNAPSHOT_CURSOR_FILE | edge-chatroom-snapshots.cursor |
| GEWE_SKILL_CHATROOM_MEMBER_EVENT_CURSOR_FILE | edge-chatroom-member-events.cursor |
| GEWE_SKILL_CHATROOM_SYSTEM_EVENT_CURSOR_FILE | edge-chatroom-system-events.cursor |

`VOLUME_OK` 内容为挂载盘的 Volume UUID。`gewe-run` 在读取 env 之前验证实际挂载点、UUID、数据目录可写与 sqlite 已存在，失败退出 75；周期任务先通过带 READ_TOKEN 的读探测，才开始同步。它对 edge 单 cursor、附件单 cursor、chatroom-events 三 cursor 做运行前快照；退出非零、JSON `ok` 非 true 或存在失败计数时恢复整组 cursor。只将固定诊断与 JSON 数值统计写入日志，原始 stderr 与正文不写日志，避免泄露凭据和消息。

包装器调用 `/opt/homebrew/bin/gtimeout --kill-after=30`，每个任务独立进程组；sync 默认 600 秒，identity/maintenance 1800 秒，radar 1200 秒，memory 常驻无超时。可重复指定 `--env` 与 `--cursor-guard`，或指定 `--timeout`。`NO_PROXY` 包含 loopback；HTTP(S)_PROXY 不设置，外部访问依赖现有网络路由。

安装器先渲染九个 plist 到 `~/srv/gewe-skill-memory/launchd/`，全部通过 `plutil -lint` 后仍不激活。`--activate` 才写入 `~/Library/LaunchAgents` 并 bootstrap 用户 GUI 域。迁移时须按冻结、校验、memory、读同步、低频任务、radar 的顺序分批 bootstrap，不能直接批量激活。

LaunchAgents 依赖用户登录会话：自动登录后启动、锁屏继续运行、登出停止。没有配置 LaunchDaemon。切换前禁止两端同时运行写 edge 的 attachment-backfill 或正式 radar。

标签：`com.wangnov.gewe-skill-memory`、`com.wangnov.gewe-sync-edge`、`com.wangnov.gewe-sync-chatroom-events`、`com.wangnov.gewe-sync-attachments`、`com.wangnov.gewe-attachment-backfill`、`com.wangnov.gewe-identity-refresh`、`com.wangnov.gewe-identity-event-backfill`、`com.wangnov.wechat-radar`、`com.wangnov.wechat-radar-identity`。

检查：`bash -n` 两个脚本，`plutil -lint` 所有 plist；运行时检查带鉴权的 doctor `memory.read_probe.ok`，不要只看 doctor 顶层 `ok`。查看日志中的 `task_completed`、`task_failed`、`cursor_restored` 固定标记。部署完成后另行安排外置盘备份、日志轮转与加密评估。
