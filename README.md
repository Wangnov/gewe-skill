# gewe-skill

`gewe-skill` 用于把 GeWe 微信回调整理成智能体（Agent）可读取、可检索、可长期保存的记忆层。

这个项目采用 Rust 优先的 monorepo 结构，包含边缘接入层、长期记忆服务、可复用 Rust 客户端，以及面向各类 Agent 的通用 Skill。

## 当前状态

当前实现重点服务于只读场景，让 Agent 能安全查看和分析微信消息，而不是直接替用户操作微信：

- 通过 Cloudflare Workers 接收 GeWe 回调。
- 将 V1/V2 回调统一规范化为共享结构。
- 在边缘侧短期保存原始回调证据。
- 在边缘侧把附件下载并写入 R2。
- 由 Rust 长期记忆服务持续拉取原始回调和附件内容。
- 通过 CLI/API 查询消息、会话、群事件和附件。
- 暂不开放微信发消息等写入或变更类 API。

## 项目组成

| 组件 | 路径 | 作用 |
| --- | --- | --- |
| gewe-skill-edge | `apps/gewe-skill-edge` | Cloudflare Workers 边缘接入层和短期缓冲区 |
| gewe-skill-memory | `crates/gewe-skill-memory` | 持久化消息记忆服务和只读 API |
| gewe-skill-client | `crates/gewe-skill-client` | 访问记忆服务的 Rust SDK |
| gewe-skill-types | `crates/gewe-skill-types` | 共享 DTO 和规范化数据结构 |
| gewe-skill-core | `crates/gewe-skill-core` | 回调规范化、事件解析和差异计算逻辑 |
| gewe-skill-cli | `crates/gewe-skill-cli` | 用于同步、补拉和检查的运维 CLI |
| gewe-skill | `skills/gewe-skill` | 通用 Agent Skill 使用说明 |

## 快速开始

构建并测试 Rust workspace：

```bash
cargo test --workspace --all-targets
```

检查 Cloudflare Worker：

```bash
cd apps/gewe-skill-edge
npm ci
npm run check
```

在本机运行记忆服务：

```bash
export GEWE_SKILL_DATABASE_URL='sqlite:/tmp/gewe-skill-memory.sqlite?mode=rwc'
export GEWE_SKILL_LISTEN='127.0.0.1:8788'
export GEWE_SKILL_READ_TOKEN='local-read-token'
export GEWE_SKILL_WRITE_TOKEN='local-write-token'
cargo run -p gewe-skill-memory
```

使用 CLI 查询数据：

```bash
export GEWE_SKILL_BASE_URL='http://127.0.0.1:8788'
export GEWE_SKILL_READ_TOKEN='local-read-token'
cargo run -p gewe-skill-cli -- health
cargo run -p gewe-skill-cli -- recent --limit 20
cargo run -p gewe-skill-cli -- search --q '<keyword>' --limit 20
cargo run -p gewe-skill-cli -- attachments --limit 20
```

## 部署模型

推荐的生产部署方式是拉取式同步：

```text
GeWe -> gewe-skill-edge -> Cloudflare D1/R2/Queue -> gewe-skill-memory -> Agent CLI/API
```

`gewe-skill-edge` 负责接收回调并保存短期证据。`gewe-skill-memory` 运行在可信服务器上，定期从边缘侧管理导出接口拉取数据。这样可以避免把记忆服务的写入 API 暴露到公网。

## Agent 使用方式

任何支持 Markdown Skill 或指令文件的 Agent runtime，都可以安装或引用 `skills/gewe-skill/SKILL.md`。这个 Skill 默认以 CLI 为核心，并且保持只读。

安装 Markdown Skill 到通用本地目录：

```bash
./scripts/install-skill.sh
```

安装到指定 Agent runtime 目录：

```bash
./scripts/install-skill.sh "$HOME/.codex/skills/gewe-skill"
```

常用命令：

```bash
gewe-skill recent --limit 50
gewe-skill conversations --limit 100
gewe-skill chatroom-events --chatroom-id '<chatroom_id>' --limit 100
gewe-skill chatroom-system-events --chatroom-id '<chatroom_id>' --limit 100
gewe-skill attachment-download --sha256 '<sha256>' --output /tmp/gewe-attachment.bin
```

## 安全边界

- 不要提交 GeWe token、回调密钥、边缘管理 token 或记忆服务 bearer token。
- 将 GeWe 回调和本地微信数据库视为源证据。
- 将 `gewe-skill-memory` 视为 Agent 可读取的记忆层。
- 除非用户明确授权具体修复或删除动作，否则保持源数据不可变。
- 未来如果加入微信写入类 API，应作为单独的、需要明确授权的能力面开放。

## 文档

- [架构说明](docs/architecture.md)
- [部署说明](docs/deployment.md)
- [API 说明](docs/api.md)
- [隐私说明](docs/privacy.md)
- [GeWe 回调笔记](docs/gewe-callbacks.md)
- [在线验证记录](docs/live-validation.md)
- [发布流程](docs/release.md)

## 许可证

MIT
