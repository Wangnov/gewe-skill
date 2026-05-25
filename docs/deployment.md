# Deployment

## Edge

`apps/gewe-skill-edge` runs on Cloudflare Workers.

Required Cloudflare resources:

- D1 database
- R2 bucket
- Queue for attachment downloads
- Worker secrets: `GEWE_TOKEN`, `CALLBACK_SECRET`, `ADMIN_API_KEY`

Copy `.dev.vars.example` for local development and set production secrets through Wrangler.

## Memory

`crates/gewe-skill-memory` is a Rust service intended to run on a small VPS or home server.

Recommended environment:

```bash
GEWE_SKILL_DATABASE_URL='sqlite:/opt/gewe-skill-memory/data/gewe-skill-memory.sqlite?mode=rwc'
GEWE_SKILL_LISTEN='127.0.0.1:8788'
GEWE_SKILL_WRITE_TOKEN='replace-with-random-write-token'
GEWE_SKILL_READ_TOKEN='replace-with-random-read-token'
```

Put Caddy, Nginx, Cloudflare Tunnel, or Tailscale in front of the service depending on your threat model.
