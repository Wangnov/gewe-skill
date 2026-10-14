# Testing

The minimal local and CI gate is:

```bash
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --locked
cargo build --locked -p gewe-skill-cli -p gewe-skill-memory --bins
python3 scripts/test-usability-2026-10-15.py
bash -n scripts/*.sh
cd apps/gewe-skill-edge
npm test
npm run check
```

CI 运行 Rust 格式/测试、临时 memory 与 CLI 端到端测试、shell 语法及 edge 测试/类型检查。Clippy 按 workspace 配置运行（pedantic 为 warn；现有代码存在历史告警，CI 不把它们升级为错误）。

Current coverage focuses on high-risk Agent-facing behavior:

- GeWe callback normalization and chatroom event extraction in `gewe-skill-core`.
- Agent chatroom event timeline filtering, deduplication, and identity display enrichment in `gewe-skill-cli`.
- Recent chatroom-event identity backfill candidate filtering in `gewe-skill-memory`.

Keep these tests lightweight and deterministic. They should not call GeWe, Cloudflare, OCI, or any live private data source.

2026-10-15 的新增验证覆盖：好友私聊优先、精确群名、dm unresolved、同时间戳分页、after 时区边界、文本显示名与非文本标记、按消息下载/多附件选择、输出文件失败保护、MIME 读取修正和 dry-run/apply、定向补拉及全局游标保护。Python 测试只起 loopback 服务，使用临时 SQLite、模拟 SSH/edge 和虚构身份；edge 测试通过 Node 内置 SQLite 执行真实 D1 SQL，并验证流式头部识别不丢字节。使用 Node 22.18+：该版本默认支持本项目测试直接导入 .ts 文件，参见 [Node TypeScript 文档](https://nodejs.org/api/typescript.html#modules-typescript)。
