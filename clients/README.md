# Clients

Shared client code for the Agent Cloud apps (Calendar, Notes, Todos). Each app keeps a copy; this folder is the source. Change it here first, test, then copy.

| File | Used by | What |
|---|---|---|
| `web/cloud-client.js` | Calendar, Todos, Notes (web) | Sign in, auth token, non-extractable seal key in IndexedDB (and the previous one after a 401, to open unsent seals once a rotated key signs in), open/seal, `changes` sync, `If-Match`, trash |
| `web/login.js` | the same | The `<cloud-login>` element (`<dottie-login>` still works) |
| `shell/cloud-lib.sh` | Calendar, Todos, Notes (Omarchy) | Token on curl's stdin, private temp dir, open/seal via the `agentcloud` binary, `If-Match` |

Neither keeps the write key: requests carry `sha256hex(key + "almanac-auth-v1")`, and only the client can derive the seal key.

Tests (after `cargo build`):

```bash
node --test clients/web/cloud-client.test.js
bash clients/shell/cloud-lib.test.sh
```
