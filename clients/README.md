# Clients

Shared client code for the Dottie apps. Each app keeps a copy; this folder is the source. Change it here first, test, then copy.

| File | Used by | What |
|---|---|---|
| `web/almanac-client.js` | Dottie Calendar, Todos, Notes (web) | Sign in, auth token, non-extractable seal key in IndexedDB, open/seal, `changes` sync, `If-Match`, trash |
| `web/login.js` | the same | The `<dottie-login>` element |
| `shell/almanac-lib.sh` | Dottie Calendar, Todos, Notes (Omarchy) | Token on curl's stdin, private temp dir, open/seal via the `almanac` binary, `If-Match` |

Neither keeps the write key: requests carry `sha256hex(key + "almanac-auth-v1")`, and only the client can derive the seal key.

Tests (after `cargo build`):

```bash
node --test clients/web/almanac-client.test.js
bash clients/shell/almanac-lib.test.sh
```
