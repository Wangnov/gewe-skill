# Testing

The minimal local and CI gate is:

```bash
cargo test --workspace --all-targets
bash -n scripts/*.sh
```

The GitHub CI workflow runs the same checks on every push.

Current coverage focuses on high-risk Agent-facing behavior:

- GeWe callback normalization and chatroom event extraction in `gewe-skill-core`.
- Agent chatroom event timeline filtering, deduplication, and identity display enrichment in `gewe-skill-cli`.
- Recent chatroom-event identity backfill candidate filtering in `gewe-skill-memory`.

Keep these tests lightweight and deterministic. They should not call GeWe, Cloudflare, OCI, or any live private data source.
