#!/usr/bin/env bash
# bash clients/shell/cloud-lib.test.sh   (needs target/debug/agentcloud)
set -u
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
BIN="${AGENTCLOUD_BIN:-${ALMANAC_BIN:-$root/target/debug/agentcloud}}"
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
  export AGENTCLOUD_CONFIG="$work/creds.json" AGENTCLOUD_BIN="$BIN"
  # shellcheck source=cloud-lib.sh
  . "$here/cloud-lib.sh"
  cloud_load || { echo "not ok - load"; exit 1; }
  check "root is the write url's calendar" "$CLOUD_ROOT" "http://127.0.0.1:$port/v1/c/$CLOUD_ID"
  check "token is not the key" "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[0]["key"])' "$CLOUD_CONFIG" | grep -c "$CLOUD_TOKEN")" "0"
  out=$(cloud_tmp)
  check "first request teaches the token (legacy calendar)" "$(cloud_curl GET "$CLOUD_ROOT" "$out")" "200"
  check "feed" "$(cloud_feed)" "seal"
  plain=$(cloud_tmp); body=$(cloud_tmp)
  echo '{"title":"Secret","body":"x","rev":9,"uid":"n1"}' >"$plain"
  cloud_seal_body note n1 "$plain" "$body"
  check "sealed body has no plaintext" "$(grep -c Secret "$body")" "0"
  check "put sealed note" "$(cloud_curl PUT "$CLOUD_ROOT/notes/n1" "$out" "$body")" "201"
  rev=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["rev"])' "$out")
  check "opened title" "$(cloud_open note <"$out" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["title"], d["sealed"], d["rev"])')" "Secret True $rev"
  check "stale If-Match is 412" "$(cloud_curl PUT "$CLOUD_ROOT/notes/n1" "$out" "$body" 0)" "412"
  check "412 current is opened" "$(cloud_open note <"$out" | python3 -c 'import json,sys; print(json.load(sys.stdin)["current"]["title"])')" "Secret"
  check "matching If-Match is 200" "$(cloud_curl PUT "$CLOUD_ROOT/notes/n1" "$out" "$body" "$rev")" "200"
  check "changes list opens" "$(cloud_curl GET "$CLOUD_ROOT/changes?since=0" "$out")" "200"
  check "changes notes" "$(cloud_open note <"$out" | python3 -c 'import json,sys; print([n["title"] for n in json.load(sys.stdin)["notes"]])')" "['Secret']"
  check "error text" "$(cloud_curl GET "$CLOUD_ROOT/notes/nope" "$out"; echo " $(cloud_error "$out")")" "404 not found"
  echo "$CLOUD_TMPDIR" >"$work/tmpdir"
  exit $fails
)
fails=$?

# Rename defaults: ~/.config/agentcloud first, ~/.config/almanac as fallback,
# and the old prod host moves to cloud.bixbyapps.com.
home=$(mktemp -d); mkdir -p "$home/almanac"
echo '[{"id":"cal_x","key":"k","write":"https://almanac.dottie.ai/v1/c/cal_x/events"}]' >"$home/almanac/hosted-calendars.json"
got=$(env -i PATH="$PATH" HOME="$home" XDG_CONFIG_HOME="$home" bash -c '. "$1"; cloud_load; echo "$CLOUD_CONFIG $CLOUD_ROOT"' _ "$here/cloud-lib.sh")
check "falls back to ~/.config/almanac, old host to cloud" "$got" "$home/almanac/hosted-calendars.json https://cloud.bixbyapps.com/v1/c/cal_x"
mkdir -p "$home/agentcloud"; cp "$home/almanac/hosted-calendars.json" "$home/agentcloud/"
got=$(env -i PATH="$PATH" HOME="$home" XDG_CONFIG_HOME="$home" bash -c '. "$1"; echo "$CLOUD_CONFIG"' _ "$here/cloud-lib.sh")
check "prefers ~/.config/agentcloud" "$got" "$home/agentcloud/hosted-calendars.json"
got=$(env -i PATH="$PATH" HOME="$home" XDG_CONFIG_HOME="$home" AGENTCLOUD_BASE=http://h:1 bash -c '. "$1"; cloud_load; echo "$CLOUD_ROOT"' _ "$here/cloud-lib.sh")
check "AGENTCLOUD_BASE wins" "$got" "http://h:1/v1/c/cal_x"
rm -rf "$home"
check "temp dir removed on exit" "$(test -e "$(cat "$work/tmpdir")" && echo left || echo gone)" "gone"
exit $fails
