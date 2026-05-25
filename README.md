# gewe-skill

`gewe-skill` turns GeWe WeChat callbacks into an agent-readable memory layer.

The project is designed as a Rust-first monorepo with an edge ingest layer, a persistent memory service, a reusable Rust client, and a universal Agent Skill.

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

## License

MIT
