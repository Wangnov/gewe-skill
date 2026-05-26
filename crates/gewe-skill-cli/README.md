# gewe-skill CLI

Agent-first JSON CLI for `gewe-skill-memory`.

```bash
gewe-skill --json doctor
gewe-skill --json conversations list --limit 50
gewe-skill --json identity resolve --q '<group-or-member-name>' --limit 10
gewe-skill --json identity refresh --chatroom-id '<chatroom_id>'
gewe-skill --json messages list --conversation-id '<conversation_id>' --limit 50
gewe-skill --json messages search --q '<keyword>' --conversation-id '<conversation_id>' --limit 20
gewe-skill --json messages context --message-key '<message_key>' --before 5 --after 5
gewe-skill --json attachments list --limit 20
gewe-skill --json attachments download --sha256 '<sha256>' --output /tmp/gewe-attachment.bin
```

Maintenance commands are intentionally separated:

```bash
gewe-skill --json ingest normalize --file callback.json --received-at 2026-05-26T00:00:00.000Z
gewe-skill --json ingest file --file callback.json --received-at 2026-05-26T00:00:00.000Z
gewe-skill --json sync edge --limit 100
gewe-skill --json sync attachments --limit 50
```
