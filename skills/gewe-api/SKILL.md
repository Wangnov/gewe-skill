---
name: gewe-api
description: Search, inspect, preview, and call any official GeWe WeChat API from natural-language requests. Use whenever the user asks to read or change live WeChat state through GeWe, including contacts, labels, messages, groups, Moments, profile, favorites, Channels, login, and account management. Handles unfamiliar GeWe capabilities without browsing docs manually.
---

# GeWe API

Use the installed `gewe-api` CLI. Do not guess endpoint paths, field names, or mutation semantics.

## Workflow

1. Check the configured Oracle runner:

```bash
gewe-api --json doctor
```

2. Search the local official capability catalog using the user's original wording:

```bash
gewe-api --json find --intent '<用户原话>'
```

3. Inspect only the selected operation. This loads that operation's OpenAPI schema, not the whole documentation set:

```bash
gewe-api --json describe '<operation_id>'
```

4. Build the smallest JSON body from `request.fields`, `request.required`, and `request.example`. Omit `appId`; the CLI injects it.

5. Call read-only operations directly:

```bash
gewe-api --json call '<operation_id>' --body '<json>'
```

6. For a mutation, first run the same command without `--execute`. Report the preview's exact targets and effects. Execute only when the user explicitly authorized that specific change:

```bash
gewe-api --json call '<operation_id>' --body '<json>' --execute --confirm '<operation_id>'
```

7. Treat `api_acknowledged` as transport/API success, not proof of final WeChat state. For consequential changes, find and call the corresponding read operation and report the verified state.

## Rules

- Prefer `find` and `describe`; do not browse GeWe documentation manually.
- Resolve human names to stable wxids before sending a body that requires IDs.
- Preserve existing values when an endpoint uses full-replacement semantics.
- Never send messages, add/delete contacts, change groups, publish content, log out, or perform another mutation without explicit authorization for that exact action.
- Never print tokens or copy them into command arguments.
- Use `request` only when the catalog is missing a newly released endpoint. Treat every raw non-GET/HEAD request as a mutation.
- If `find` is ambiguous, show the candidates and ask for a narrower clue; do not guess.

## Examples

```bash
gewe-api --json find --intent '给好友打标签'
gewe-api --json describe api-139908354
gewe-api --json find --intent '查看一个群的公告'
```

Refresh the compact catalog only when an operation is missing or the official docs changed:

```bash
gewe-api --json catalog sync
```
