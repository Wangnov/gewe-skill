# gewe-skill-edge

Cloudflare Workers edge ingest for `gewe-skill`.

## Responsibilities

- Receive GeWe callback events.
- Normalize V1/V2 callback payloads into a short-term D1 buffer.
- Persist raw callback JSON and downloaded attachments into R2.
- Queue attachment downloads through Cloudflare Queues.
- Keep only short TTL data at the edge; durable memory belongs to `gewe-skill-memory`.
- Expose admin export endpoints for pull-based snapshot, event, and attachment sync into `gewe-skill-memory`.

## Secrets

Create local `.dev.vars` from `.dev.vars.example` and set Cloudflare secrets for production:

- `GEWE_TOKEN`
- `CALLBACK_SECRET`
- `ADMIN_API_KEY`

Never commit real tokens or app identifiers.

## Sync model

The recommended production path is pull-based. Run `gewe-skill sync-edge` and `gewe-skill sync-edge-attachments` from the trusted memory server using `ADMIN_API_KEY`.

Direct push to `gewe-skill-memory` is supported only as an optional mode by setting both `MEMORY_API_URL` and `MEMORY_WRITE_TOKEN`. Avoid this unless you intentionally expose a hardened write endpoint for memory.
