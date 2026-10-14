# gewe-api

`gewe-api` is the deterministic execution layer for the companion `gewe-api` Codex skill. It searches the compact official GeWe catalog, loads one endpoint's OpenAPI schema on demand, validates request bodies, and forwards live calls to the configured Oracle runner without copying GeWe credentials to the Mac.

```bash
gewe-api --json doctor
gewe-api --json find --intent '查看好友标签'
gewe-api --json describe api-139908353
gewe-api --json call api-139908353 --body '{}'
```

Mutation calls preview by default. After explicit user authorization, the caller must provide both flags:

```bash
gewe-api --json call <operation_id> --body '<json>' \
  --execute --confirm <operation_id>
```

## JSON contract

- Successful commands emit one JSON object to stdout and exit zero.
- Errors emit `{"ok":false,"error":{"message":"..."}}` and exit nonzero.
- Live API success uses `status=api_acknowledged`; this does not claim final-state verification.
- Credentials and authentication headers are never emitted.

## Install

```bash
make -C crates/gewe-api-cli install-local
```
