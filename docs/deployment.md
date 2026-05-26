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

On an OCI-style Linux host, install or update from GitHub Release assets:

```bash
sudo GEWE_SKILL_VERSION=v0.1.18 scripts/install-oci-memory-release.sh
```

This installs `gewe-skill-memory`, `gewe-skill`, the service unit, and the recommended sync/maintenance timers without compiling on the server.

Recommended environment:

```bash
GEWE_SKILL_DATABASE_URL='sqlite:/opt/gewe-skill-memory/data/gewe-skill-memory.sqlite?mode=rwc'
GEWE_SKILL_LISTEN='127.0.0.1:8788'
GEWE_SKILL_WRITE_TOKEN='replace-with-random-write-token'
GEWE_SKILL_READ_TOKEN='replace-with-random-read-token'
GEWE_SKILL_ATTACHMENT_DIR='/opt/gewe-skill-memory/data/attachments'
GEWE_SKILL_GEWE_BASE_URL='http://api.geweapi.com'
GEWE_SKILL_GEWE_APP_ID='replace-with-gewe-app-id'
GEWE_SKILL_GEWE_TOKEN='replace-with-gewe-token'
```

Put Caddy, Nginx, Cloudflare Tunnel, or Tailscale in front of the service depending on your threat model.

## Pull sync from edge

When the memory service is not publicly reachable, run pull sync on the server instead of pushing from Cloudflare Workers:

```bash
gewe-skill --json sync edge
gewe-skill --json sync attachments
gewe-skill --json sync chatroom-events
```

The systemd timer templates under `crates/gewe-skill-memory/deploy/` keep raw callbacks, chatroom events, attachment bytes, and recent identity data synchronized into memory.

## Identity refresh

The memory service can also call GeWe read-only APIs to build a durable identity index:

```bash
gewe-skill --json identity refresh --recent-chatrooms 20
gewe-skill --json identity refresh --chatroom-id '<chatroom_id>'
gewe-skill --json identity resolve --q '<group or member name>'
gewe-skill --json maintenance identity-backfill --event-limit 500 --max-chatrooms 10 --max-wxids 100
```

Keep GeWe credentials only in the memory service environment. Agent runtimes should call `gewe-skill` through the local memory API instead of calling GeWe directly.

Prefer bounded maintenance over broad polling. `identity-backfill` scans recent chatroom events, refreshes only the related chatrooms, then refreshes only still-missing wxids.
