# Agent Cloud shell client library. Source it; do not run it.
# Copied into each Omarchy app; this file in the agentcloud repo is the source.
#
#   . "$(dirname "$0")/cloud-lib.sh"
#   cloud_load [CAL_ID] || exit       # sets CLOUD_ID, CLOUD_ROOT, CLOUD_TOKEN
#   out=$(cloud_tmp)
#   code=$(cloud_curl GET "$CLOUD_ROOT/notes" "$out")
#
# The write key never enters a shell variable, argv, or a log. Requests carry
# the auth token, SHA-256(key + "almanac-auth-v1"), passed to curl on stdin
# (`-K -`), so `ps` cannot see it either. Temp files live in one private
# directory that is removed on exit. ("almanac-auth-v1" is the wire format and
# must not be renamed.)
#
# Env (first set wins): CLOUD_CONFIG, AGENTCLOUD_CONFIG, CLOUD_CONFIG
# (credentials file); AGENTCLOUD_BASE, CLOUD_BASE; CLOUD_UA, AGENTCLOUD_UA,
# CLOUD_UA; AGENTCLOUD_BIN, ALMANAC_BIN. The ALMANAC_* names are read only
# for installs from before the rename.

_cloud_cfg="${XDG_CONFIG_HOME:-$HOME/.config}"
CLOUD_CONFIG="${CLOUD_CONFIG:-${AGENTCLOUD_CONFIG:-${CLOUD_CONFIG-}}}"
if [[ -z $CLOUD_CONFIG ]]; then
  CLOUD_CONFIG="$_cloud_cfg/agentcloud/hosted-calendars.json"
  # Installs from before the rename keep their credentials in ~/.config/almanac.
  if [[ ! -f $CLOUD_CONFIG && -f $_cloud_cfg/almanac/hosted-calendars.json ]]; then
    CLOUD_CONFIG="$_cloud_cfg/almanac/hosted-calendars.json"
  fi
fi
unset _cloud_cfg
# Set when the caller chose a server; otherwise the credentials' write URL
# decides, with the old almanac.dottie.ai host moved to the Agent Cloud host.
CLOUD_BASE_SET="${AGENTCLOUD_BASE:+1}${CLOUD_BASE:+1}"
CLOUD_BASE="${AGENTCLOUD_BASE:-${CLOUD_BASE:-https://cloud.bixbyapps.com}}"
CLOUD_UA="${CLOUD_UA:-${AGENTCLOUD_UA:-${CLOUD_UA:-Mozilla/5.0 agentcloud-omarchy}}}"
CLOUD_TMPDIR=""

# A new temp file in this run's private directory (made by cloud_load).
cloud_tmp() {
  [[ -n $CLOUD_TMPDIR && -d $CLOUD_TMPDIR ]] || return 1
  mktemp "$CLOUD_TMPDIR/f.XXXXXX"
}

# Must run in the script's own shell, not in $(...): it sets the trap.
_cloud_init_tmp() {
  [[ -n $CLOUD_TMPDIR ]] && return 0
  CLOUD_TMPDIR=$(mktemp -d "${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}}/agentcloud.XXXXXX") || return 1
  chmod 700 "$CLOUD_TMPDIR"
  local prev
  prev=$(trap -p EXIT | sed -E "s/^trap -- '(.*)' EXIT$/\1/")
  # shellcheck disable=SC2064
  trap "rm -rf '$CLOUD_TMPDIR'${prev:+; $prev}" EXIT
}

# Read the calendar from the credentials file and make the private temp dir.
# Call it directly (not in $(...)). Prints nothing secret.
cloud_load() {
  local want="${1-}" out
  [[ -f $CLOUD_CONFIG ]] || return 1
  _cloud_init_tmp || return 1
  out=$(CLOUD_WANT="$want" python3 - "$CLOUD_CONFIG" <<'PY'
import hashlib, json, sys, os
try:
    raw = json.load(open(sys.argv[1]))
except Exception:
    sys.exit(1)
rows = raw if isinstance(raw, list) else raw.get("calendars", [raw]) if isinstance(raw, dict) else []
want = os.environ.get("CLOUD_WANT", "").strip()
for row in rows:
    if not isinstance(row, dict):
        continue
    cid, key = str(row.get("id") or ""), str(row.get("key") or "")
    if not cid or not key or (want and cid != want):
        continue
    token = hashlib.sha256((key + "almanac-auth-v1").encode()).hexdigest()
    print(cid)
    print(str(row.get("write") or ""))
    print(token)
    sys.exit(0)
sys.exit(1)
PY
) || return 1
  CLOUD_ID=$(sed -n 1p <<<"$out")
  local write
  write=$(sed -n 2p <<<"$out")
  CLOUD_TOKEN=$(sed -n 3p <<<"$out")
  if [[ -z $CLOUD_BASE_SET && $write == */v1/c/*/events* && $write != https://almanac.dottie.ai/* ]]; then
    CLOUD_ROOT="${write%/events*}"
  else
    CLOUD_ROOT="${CLOUD_BASE%/}/v1/c/${CLOUD_ID}"
  fi
  [[ -n $CLOUD_ID && -n $CLOUD_TOKEN ]]
}

# cloud_curl METHOD URL OUTFILE [BODYFILE] [IF_MATCH_REV] -> prints the HTTP code.
# A 401 on a calendar from before 0.30.0 is retried once after a raw-key
# request teaches the server the token. The key goes from the file to curl's
# stdin inside python, never through the shell.
cloud_curl() {
  local method="$1" url="$2" out="$3" body="${4-}" rev="${5-}" code
  code=$(_cloud_curl_token "$method" "$url" "$out" "$body" "$rev") || return 1
  if [[ $code == 401 ]] && _cloud_teach_token; then
    code=$(_cloud_curl_token "$method" "$url" "$out" "$body" "$rev") || return 1
  fi
  printf '%s' "$code"
}

_cloud_curl_token() {
  local method="$1" url="$2" out="$3" body="${4-}" rev="${5-}"
  local args=(-sS --max-time 20 -A "$CLOUD_UA" -o "$out" -w '%{http_code}' -X "$method" -K -)
  [[ -n $body ]] && args+=(-H 'Content-Type: application/json' --data-binary "@$body")
  [[ -n $rev ]] && args+=(-H "If-Match: \"$rev\"")
  printf 'header = "Authorization: Bearer %s"\n' "$CLOUD_TOKEN" | curl "${args[@]}" "$url"
}

_cloud_teach_token() {
  [[ -n ${_CLOUD_TAUGHT-} ]] && return 1
  _CLOUD_TAUGHT=1
  local code
  code=$(CLOUD_WANT="$CLOUD_ID" python3 - "$CLOUD_CONFIG" <<'PY' | curl -sS --max-time 20 -A "$CLOUD_UA" -o /dev/null -w '%{http_code}' -K - "$CLOUD_ROOT"
import json, os, sys
raw = json.load(open(sys.argv[1]))
rows = raw if isinstance(raw, list) else raw.get("calendars", [raw])
for row in rows:
    if isinstance(row, dict) and row.get("id") == os.environ["CLOUD_WANT"]:
        print('header = "Authorization: Bearer %s"' % row["key"])
        break
PY
)
  [[ $code == 200 ]]
}

# `feed` of the calendar: seal or plain.
cloud_feed() {
  local out code
  out=$(cloud_tmp) || return 1
  code=$(cloud_curl GET "$CLOUD_ROOT" "$out") || return 1
  [[ $code == 200 ]] || return 1
  python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d.get("feed") or "plain")' "$out"
}

_cloud_supports_seal() {
  [[ -x $1 ]] && LC_ALL=C grep -aq 'almanac-auth-v1' "$1"
}

# The agentcloud binary (or an older almanac 0.30.0+ build), for sealed calendars.
cloud_bin() {
  local b
  for b in "${AGENTCLOUD_BIN-}" "${ALMANAC_BIN-}" \
    "$(command -v agentcloud 2>/dev/null)" "$(command -v almanac 2>/dev/null)" \
    "$HOME/Projects/agentcloud/target/release/agentcloud" "$HOME/Projects/agentcloud/target/debug/agentcloud" \
    "$HOME/Projects/almanac/target/release/agentcloud" "$HOME/Projects/almanac/target/debug/agentcloud" \
    "$HOME/Projects/almanac/target/release/almanac" "$HOME/Projects/almanac/target/debug/almanac"; do
    if [[ -n $b ]] && _cloud_supports_seal "$b"; then
      printf '%s\n' "$b"
      return 0
    fi
  done
  return 1
}

# cloud_open KIND < json > json
# Opens every sealed item in a response: one item, a list under "events",
# "todos" or "notes", or a changes feed. Plain items pass through. Server
# fields (uid, rev, createdAt, updatedAt) are kept; `sealed: true` is added.
cloud_open() {
  local kind="$1" bin
  bin=$(cloud_bin) || bin=""
  AGENTCLOUD_CREDS="$CLOUD_CONFIG" ALMANAC_CREDS="$CLOUD_CONFIG" OPEN_BIN="$bin" OPEN_CAL="$CLOUD_ID" OPEN_KIND="$kind" python3 -c '
import json, os, subprocess, sys
data = json.load(sys.stdin)
binary, cal = os.environ["OPEN_BIN"], os.environ["OPEN_CAL"]
KEEP = ("uid", "rev", "sequence", "createdAt", "updatedAt")
def open_one(kind, item):
    if not isinstance(item, dict) or not item.get("seal"):
        return item
    if not binary:
        sys.stderr.write("sealed calendar needs the agentcloud binary\n")
        sys.exit(2)
    proc = subprocess.run([binary, "open", "--cal", cal, "--kind", kind, "--uid", str(item.get("uid") or "")],
                          input=str(item["seal"]).encode(), capture_output=True)
    if proc.returncode != 0:
        return {"uid": item.get("uid"), "rev": item.get("rev"), "unreadable": True}
    plain = json.loads(proc.stdout or b"{}")
    plain = plain if isinstance(plain, dict) else {}
    plain.pop("seal", None)
    for k in KEEP:
        if k in item:
            plain[k] = item[k]
    plain["sealed"] = True
    return plain
lists = {"events": "event", "todos": "todo", "notes": "note"}
if isinstance(data, dict) and any(isinstance(data.get(k), list) for k in lists):
    for name, kind in lists.items():
        if isinstance(data.get(name), list):
            data[name] = [open_one(kind, i) for i in data[name]]
elif isinstance(data, dict) and isinstance(data.get("current"), dict):
    data["current"] = open_one(os.environ["OPEN_KIND"], data["current"])
else:
    data = open_one(os.environ["OPEN_KIND"], data)
json.dump(data, sys.stdout)
sys.stdout.write("\n")
'
}

# cloud_seal_body KIND UID PLAIN_JSON_FILE OUT_FILE: writes {"seal": "alm1..."}.
# Server fields in the plain JSON are dropped before sealing.
cloud_seal_body() {
  local kind="$1" uid="$2" plain="$3" out="$4" bin clean
  bin=$(cloud_bin) || return 1
  clean=$(cloud_tmp) || return 1
  python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
for k in ("uid", "seal", "rev", "sequence", "createdAt", "updatedAt", "sealed", "unreadable"):
    d.pop(k, None)
json.dump(d, open(sys.argv[2], "w"))' "$plain" "$clean" || return 1
  local wire
  # Not a pipe: a failed seal must fail here, not write {"seal": ""}.
  wire=$(AGENTCLOUD_CREDS="$CLOUD_CONFIG" ALMANAC_CREDS="$CLOUD_CONFIG" "$bin" seal --cal "$CLOUD_ID" --kind "$kind" --uid "$uid" <"$clean") || return 1
  [[ $wire == alm1.* ]] || return 1
  printf '%s' "$wire" |
    python3 -c 'import json,sys; json.dump({"seal": sys.stdin.read().strip()}, open(sys.argv[1], "w"))' "$out" || return 1
  [[ -s $out ]]
}

# The `error` text of a JSON error body, or nothing.
cloud_error() {
  python3 -c 'import json,sys
try:
    d = json.load(open(sys.argv[1]))
    print(d.get("error") or "") if isinstance(d, dict) else None
except Exception:
    pass' "$1" 2>/dev/null
}
