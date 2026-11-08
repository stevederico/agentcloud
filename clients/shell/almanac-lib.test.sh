#!/usr/bin/env bash
# bash clients/shell/almanac-lib.test.sh   (needs target/debug/almanac)
set -u
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
BIN="${ALMANAC_BIN:-$root/target/debug/almanac}"
work=$(mktemp -d); chmod 700 "$work"
port=$((30000 + RANDOM % 20000))
AGENT_KEY=h FEED_TOKEN=f PORT=$port HOST=127.0.0.1 DB_PATH="$work/c.db" BACKUP_INTERVAL_HOURS=0 "$BIN" >/dev/null 2>&1 &
pid=$!
trap 'kill $pid 2>/dev/null; rm -rf "$work"' EXIT
for _ in $(seq 100); do curl -s -o /dev/null "http://127.0.0.1:$port/health" && break; sleep 0.05; done
fails=0
check() { if [[ $2 == "$3" ]]; then echo "ok - $1"; else echo "not ok - $1: got [$2] want [$3]"; fails=$((fails + 1)); fi; }

# A legacy calendar (server-made key, so the token is not known yet).
curl -s -H 'content-type: application/json' -H 'accept: application/json' -d '{"feed":"seal"}' "http://127.0.0.1:$port/calendars" >"$work/cal.json"
python3 -c 'import json,sys; c=json.load(open(sys.argv[1])); json.dump([{"id":c["id"],"key":c["key"],"write":c["write"]}], open(sys.argv[2],"w"))' "$work/cal.json" "$work/creds.json"
chmod 600 "$work/creds.json"

(
  trap - EXIT # a subshell would otherwise chain the parent's cleanup into ours
  export ALMANAC_CONFIG="$work/creds.json" ALMANAC_BIN="$BIN"
  # shellcheck source=almanac-lib.sh
  . "$here/almanac-lib.sh"
  almanac_load || { echo "not ok - load"; exit 1; }
  check "root is the write url's calendar" "$ALMANAC_ROOT" "http://127.0.0.1:$port/v1/c/$ALMANAC_ID"
  check "token is not the key" "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[0]["key"])' "$ALMANAC_CONFIG" | grep -c "$ALMANAC_TOKEN")" "0"
  out=$(almanac_tmp)
  check "first request teaches the token (legacy calendar)" "$(almanac_curl GET "$ALMANAC_ROOT" "$out")" "200"
  check "feed" "$(almanac_feed)" "seal"
  plain=$(almanac_tmp); body=$(almanac_tmp)
  echo '{"title":"Secret","body":"x","rev":9,"uid":"n1"}' >"$plain"
  almanac_seal_body note n1 "$plain" "$body"
  check "sealed body has no plaintext" "$(grep -c Secret "$body")" "0"
  check "put sealed note" "$(almanac_curl PUT "$ALMANAC_ROOT/notes/n1" "$out" "$body")" "201"
  rev=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["rev"])' "$out")
  check "opened title" "$(almanac_open note <"$out" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["title"], d["sealed"], d["rev"])')" "Secret True $rev"
  check "stale If-Match is 412" "$(almanac_curl PUT "$ALMANAC_ROOT/notes/n1" "$out" "$body" 0)" "412"
  check "412 current is opened" "$(almanac_open note <"$out" | python3 -c 'import json,sys; print(json.load(sys.stdin)["current"]["title"])')" "Secret"
  check "matching If-Match is 200" "$(almanac_curl PUT "$ALMANAC_ROOT/notes/n1" "$out" "$body" "$rev")" "200"
  check "changes list opens" "$(almanac_curl GET "$ALMANAC_ROOT/changes?since=0" "$out")" "200"
  check "changes notes" "$(almanac_open note <"$out" | python3 -c 'import json,sys; print([n["title"] for n in json.load(sys.stdin)["notes"]])')" "['Secret']"
  check "error text" "$(almanac_curl GET "$ALMANAC_ROOT/notes/nope" "$out"; echo " $(almanac_error "$out")")" "404 not found"
  echo "$ALMANAC_TMPDIR" >"$work/tmpdir"
  exit $fails
)
fails=$?
check "temp dir removed on exit" "$(test -e "$(cat "$work/tmpdir")" && echo left || echo gone)" "gone"
exit $fails
