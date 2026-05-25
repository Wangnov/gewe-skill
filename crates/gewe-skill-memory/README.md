# gewe-skill-memory

Rust persistent memory service for `gewe-skill`.

## Responsibilities

- Accept normalized events from `gewe-skill-edge` through `POST /write/events`.
- Store long-lived messages and chatroom events in SQLite WAL mode.
- Expose read-only Agent APIs under `/api/*`.
- Keep future write-to-WeChat operations outside the current API surface.

## Environment

- `GEWE_SKILL_DATABASE_URL`: defaults to `sqlite:/opt/gewe-skill-memory/data/gewe-skill-memory.sqlite?mode=rwc`
- `GEWE_SKILL_LISTEN`: defaults to `127.0.0.1:8788`
- `GEWE_SKILL_WRITE_TOKEN`: bearer token for `/write/*`
- `GEWE_SKILL_READ_TOKEN`: bearer token for `/api/*`
