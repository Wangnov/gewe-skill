# gewe-skill-edge

Cloudflare Workers edge ingest for `gewe-skill`.

## Responsibilities

- Receive GeWe callback events.
- Normalize V1/V2 callback payloads into a short-term D1 buffer.
- Persist raw callback JSON and downloaded attachments into R2.
- Queue attachment downloads through Cloudflare Queues.
- Keep only short TTL data at the edge; durable memory belongs to `gewe-skill-memory`.

## Secrets

Create local `.dev.vars` from `.dev.vars.example` and set Cloudflare secrets for production:

- `GEWE_TOKEN`
- `CALLBACK_SECRET`
- `ADMIN_API_KEY`

Never commit real tokens or app identifiers.
