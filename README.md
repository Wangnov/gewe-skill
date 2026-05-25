# gewe-skill

`gewe-skill` turns GeWe WeChat callbacks into an agent-readable memory layer.

The project is designed as a Rust-first monorepo with an edge ingest layer, a persistent memory service, a reusable Rust client, and a universal Agent Skill.

## Status

The current implementation focuses on read-only Agent use:

- Receive GeWe callbacks through Cloudflare Workers.
- Normalize V1/V2 callback payloads into a shared schema.
- Preserve raw callback evidence in short-term edge storage.
- Download attachments into R2 at the edge.
- Pull raw callbacks and attachment bytes into a long-lived Rust memory service.
- Query messages, conversations, chatroom events, and attachments through CLI/API.
- Keep WeChat send-message and other mutation APIs out of the current surface.

## Components

| Component | Path | Role |
| --- | --- | --- |
| gewe-skill-edge | `apps/gewe-skill-edge` | Cloudflare Workers edge ingest and short-term buffer |
| gewe-skill-memory | `crates/gewe-skill-memory` | Persistent message memory service and read-only API |
| gewe-skill-client | `crates/gewe-skill-client` | Rust SDK for the memory API |
| gewe-skill-types | `crates/gewe-skill-types` | Shared DTOs and normalized schemas |
| gewe-skill-core | `crates/gewe-skill-core` | Callback normalization, event parsing, and diff logic |
| gewe-skill-cli | `crates/gewe-skill-cli` | Operations CLI for sync, backfill, and inspection |
| gewe-skill | `skills/gewe-skill` | Universal Agent Skill instructions |

## Quick start

Build and test the Rust workspace:

```bash
cargo test --workspace --all-targets
```

Check the Cloudflare Worker:

```bash
cd apps/gewe-skill-edge
npm ci
npm run check
```

Run the memory service locally:

```bash
export GEWE_SKILL_DATABASE_URL='sqlite:/tmp/gewe-skill-memory.sqlite?mode=rwc'
export GEWE_SKILL_LISTEN='127.0.0.1:8788'
export GEWE_SKILL_READ_TOKEN='local-read-token'
export GEWE_SKILL_WRITE_TOKEN='local-write-token'
cargo run -p gewe-skill-memory
```

Use the CLI:

```bash
export GEWE_SKILL_BASE_URL='http://127.0.0.1:8788'
export GEWE_SKILL_READ_TOKEN='local-read-token'
cargo run -p gewe-skill-cli -- health
cargo run -p gewe-skill-cli -- recent --limit 20
cargo run -p gewe-skill-cli -- search --q '<keyword>' --limit 20
cargo run -p gewe-skill-cli -- attachments --limit 20
```

## Deployment model

The recommended production path is pull-based:

```text
GeWe -> gewe-skill-edge -> Cloudflare D1/R2/Queue -> gewe-skill-memory -> Agent CLI/API
```

`gewe-skill-edge` receives callbacks and stores short-term evidence. `gewe-skill-memory` runs on a trusted server and periodically pulls from the edge admin export, which avoids exposing the memory write API to the public internet.

## Agent usage

Install or reference `skills/gewe-skill/SKILL.md` from any Agent runtime that supports Markdown skills/instructions. The skill is intentionally CLI-first and read-only by default.

Common commands:

```bash
gewe-skill recent --limit 50
gewe-skill conversations --limit 100
gewe-skill chatroom-events --chatroom-id '<chatroom_id>' --limit 100
gewe-skill chatroom-system-events --chatroom-id '<chatroom_id>' --limit 100
gewe-skill attachment-download --sha256 '<sha256>' --output /tmp/gewe-attachment.bin
```

## Security boundary

- Do not commit GeWe tokens, callback secrets, edge admin tokens, or memory bearer tokens.
- Treat GeWe callbacks and local WeChat databases as source evidence.
- Treat `gewe-skill-memory` as the Agent-readable memory layer.
- Keep source data immutable unless the user explicitly authorizes a narrow repair or deletion.
- Add future WeChat mutation APIs as a separate, explicitly authorized surface.

## Documentation

- [Architecture](docs/architecture.md)
- [Deployment](docs/deployment.md)
- [API](docs/api.md)
- [Privacy](docs/privacy.md)
- [GeWe callback notes](docs/gewe-callbacks.md)

## License

MIT
