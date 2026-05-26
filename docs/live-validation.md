# Live validation

This document records non-secret validation evidence from the current production-style deployment.

Do not add GeWe tokens, callback secrets, memory bearer tokens, edge admin tokens, or raw private message payloads to this file.

## 2026-05-26

Environment:

- Edge URL: `https://gewe-agent.wangnov-ai.com`
- Edge runtime: Cloudflare Workers
- Edge deployed worker: existing callback worker `gewe-wechat-ingest`
- Project worker name: `gewe-skill-edge`
- Memory host: OCI `us-oci-sjc-01`
- Memory listen address: `127.0.0.1:8788`
- Memory service: `gewe-skill-memory.service`
- Raw callback sync: `gewe-skill-edge-sync.timer`
- Attachment sync: `gewe-skill-edge-attachment-sync.timer`
- Chatroom event sync: `gewe-skill-edge-chatroom-event-sync.timer`
- Recent event identity backfill: `gewe-skill-identity-event-backfill.timer`

Verified:

- `cargo test --workspace --all-targets` passes locally.
- `npm run check` passes in `apps/gewe-skill-edge`.
- `npx wrangler deploy --dry-run` passes for the open-source edge config.
- Live edge `/health` returns `service = gewe-skill-edge`.
- Live edge deployment succeeded with version `6c46b5fe-8064-45d2-b501-ed0ebef662b9`.
- `gewe-skill-memory.service` is `active`.
- `gewe-skill-edge-sync.timer` is `active`.
- `gewe-skill-edge-attachment-sync.timer` is `active`.
- `gewe-skill-edge-chatroom-event-sync.timer` is `active`.
- `gewe-skill-identity-event-backfill.timer` is `active`.
- Memory `/healthz` returns `service = gewe-skill-memory`.
- Agent read API returns recent real callback messages for expected app id `wx_uprGq1Pp7eCZ4gZpTRNpz`.
- Agent attachment API returns synced real attachment metadata with non-null local memory `id`, edge job id, sha256, size, and MIME.
- `gewe-skill --json attachments download` successfully downloaded a real synced emoji attachment from memory to a local file on the server.

Known deployment note:

- The live Cloudflare Worker still uses the historical worker name `gewe-wechat-ingest` to preserve the existing GeWe callback route. The open-source project and reusable Worker package remain named `gewe-skill-edge`.
