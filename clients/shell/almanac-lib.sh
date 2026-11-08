# Almanac shell client library. Source it; do not run it.
# Copied into each Dottie Omarchy app; this file in the almanac repo is the source.
#
#   . "$(dirname "$0")/almanac-lib.sh"
#   almanac_load [CAL_ID] || exit       # sets ALMANAC_ID, ALMANAC_ROOT, ALMANAC_TOKEN
#   out=$(almanac_tmp)
#   code=$(almanac_curl GET "$ALMANAC_ROOT/notes" "$out")
#
# The write key never enters a shell variable, argv, or a log. Requests carry
# the auth token, SHA-256(key + "almanac-auth-v1"), passed to curl on stdin
# (`-K -`), so `ps` cannot see it either. Temp files live in one private
# directory that is removed on exit.
#
# Env: ALMANAC_CONFIG (credentials file), ALMANAC_BASE, ALMANAC_UA, ALMANAC_BIN.

ALMANAC_CONFIG="${ALMANAC_CONFIG:-${XDG_CONFIG_HOME:-$HOME/.config}/almanac/hosted-calendars.json}"
ALMANAC_BASE="${ALMANAC_BASE:-https://almanac.dottie.ai}"
ALMANAC_UA="${ALMANAC_UA:-Mozilla/5.0 dottie-omarchy}"
ALMANAC_TMPDIR=""

# A new temp file in this run's private directory (made by almanac_load).
almanac_tmp() {
  [[ -n $ALMANAC_TMPDIR && -d $ALMANAC_TMPDIR ]] || return 1
  mktemp "$ALMANAC_TMPDIR/f.XXXXXX"
}

# Must run in the script's own shell, not in $(...): it sets the trap.
_almanac_init_tmp() {
  [[ -n $ALMANAC_TMPDIR ]] && return 0
  ALMANAC_TMPDIR=$(mktemp -d "${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}}/almanac.XXXXXX") || return 1
  chmod 700 "$ALMANAC_TMPDIR"
  local prev
  prev=$(trap -p EXIT | sed -E "s/^trap -- '(.*)' EXIT$/\1/")
  # shellcheck disable=SC2064
  trap "rm -rf '$ALMANAC_TMPDIR'${prev:+; $prev}" EXIT
}

# Read the calendar from the credentials file and make the private temp dir.
# Call it directly (not in $(...)). Prints nothing secret.
almanac_load() {
  local want="${1-}" out
  [[ -f $ALMANAC_CONFIG ]] || return 1
  _almanac_init_tmp || return 1
  out=$(ALMANAC_WANT="$want" python3 - "$ALMANAC_CONFIG" <<'PY'
import hashlib, json, sys, os
try:
    raw = json.load(open(sys.argv[1]))
except Exception:
    sys.exit(1)
rows = raw if isinstance(raw, list) else raw.get("calendars", [raw]) if isinstance(raw, dict) else []
want = os.environ.get("ALMANAC_WANT", "").strip()
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
  ALMANAC_ID=$(sed -n 1p <<<"$out")
  local write
  write=$(sed -n 2p <<<"$out")
  ALMANAC_TOKEN=$(sed -n 3p <<<"$out")
  if [[ $write == */v1/c/*/events* ]]; then
    ALMANAC_ROOT="${write%/events*}"
  else
    ALMANAC_ROOT="${ALMANAC_BASE%/}/v1/c/${ALMANAC_ID}"
  fi
  [[ -n $ALMANAC_ID && -n $ALMANAC_TOKEN ]]
}

# almanac_curl METHOD URL OUTFILE [BODYFILE] [IF_MATCH_REV] -> prints the HTTP code.
# A 401 on a calendar from before 0.30.0 is retried once after a raw-key
# request teaches the server the token. The key goes from the file to curl's
# stdin inside python, never through the shell.
almanac_curl() {
  local method="$1" url="$2" out="$3" body="${4-}" rev="${5-}" code
  code=$(_almanac_curl_token "$method" "$url" "$out" "$body" "$rev") || return 1
  if [[ $code == 401 ]] && _almanac_teach_token; then
    code=$(_almanac_curl_token "$method" "$url" "$out" "$body" "$rev") || return 1
  fi
  printf '%s' "$code"
}

_almanac_curl_token() {
  local method="$1" url="$2" out="$3" body="${4-}" rev="${5-}"
  local args=(-sS --max-time 20 -A "$ALMANAC_UA" -o "$out" -w '%{http_code}' -X "$method" -K -)
  [[ -n $body ]] && args+=(-H 'Content-Type: application/json' --data-binary "@$body")
  [[ -n $rev ]] && args+=(-H "If-Match: \"$rev\"")
  printf 'header = "Authorization: Bearer %s"\n' "$ALMANAC_TOKEN" | curl "${args[@]}" "$url"
}

_almanac_teach_token() {
  [[ -n ${_ALMANAC_TAUGHT-} ]] && return 1
  _ALMANAC_TAUGHT=1
  local code
  code=$(ALMANAC_WANT="$ALMANAC_ID" python3 - "$ALMANAC_CONFIG" <<'PY' | curl -sS --max-time 20 -A "$ALMANAC_UA" -o /dev/null -w '%{http_code}' -K - "$ALMANAC_ROOT"
import json, os, sys
raw = json.load(open(sys.argv[1]))
rows = raw if isinstance(raw, list) else raw.get("calendars", [raw])
for row in rows:
    if isinstance(row, dict) and row.get("id") == os.environ["ALMANAC_WANT"]:
        print('header = "Authorization: Bearer %s"' % row["key"])
        break
PY
)
  [[ $code == 200 ]]
}

# `feed` of the calendar: seal or plain.
almanac_feed() {
  local out code
  out=$(almanac_tmp) || return 1
  code=$(almanac_curl GET "$ALMANAC_ROOT" "$out") || return 1
  [[ $code == 200 ]] || return 1
  python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d.get("feed") or "plain")' "$out"
}

_almanac_supports_seal() {
  [[ -x $1 ]] && LC_ALL=C grep -aq 'almanac-auth-v1' "$1"
}

# The almanac binary (0.30.0 or later), for sealed calendars.
almanac_bin() {
  local b
  for b in "${ALMANAC_BIN-}" "$(command -v almanac 2>/dev/null)" \
    "$HOME/Projects/almanac/target/release/almanac" "$HOME/Projects/almanac/target/debug/almanac"; do
    if [[ -n $b ]] && _almanac_supports_seal "$b"; then
      printf '%s\n' "$b"
      return 0
    fi
  done
  return 1
}

# almanac_open KIND < json > json
# Opens every sealed item in a response: one item, a list under "events",
# "todos" or "notes", or a changes feed. Plain items pass through. Server
# fields (uid, rev, createdAt, updatedAt) are kept; `sealed: true` is added.
almanac_open() {
  local kind="$1" bin
  bin=$(almanac_bin) || bin=""
  ALMANAC_CREDS="$ALMANAC_CONFIG" OPEN_BIN="$bin" OPEN_CAL="$ALMANAC_ID" OPEN_KIND="$kind" python3 -c '
import json, os, subprocess, sys
data = json.load(sys.stdin)
binary, cal = os.environ["OPEN_BIN"], os.environ["OPEN_CAL"]
KEEP = ("uid", "rev", "sequence", "createdAt", "updatedAt")
def open_one(kind, item):
    if not isinstance(item, dict) or not item.get("seal"):
        return item
    if not binary:
        sys.stderr.write("sealed calendar needs the almanac binary\n")
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

# almanac_seal_body KIND UID PLAIN_JSON_FILE OUT_FILE: writes {"seal": "alm1..."}.
# Server fields in the plain JSON are dropped before sealing.
almanac_seal_body() {
  local kind="$1" uid="$2" plain="$3" out="$4" bin clean
  bin=$(almanac_bin) || return 1
  clean=$(almanac_tmp) || return 1
  python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
for k in ("uid", "seal", "rev", "sequence", "createdAt", "updatedAt", "sealed", "unreadable"):
    d.pop(k, None)
json.dump(d, open(sys.argv[2], "w"))' "$plain" "$clean" || return 1
  ALMANAC_CREDS="$ALMANAC_CONFIG" "$bin" seal --cal "$ALMANAC_ID" --kind "$kind" --uid "$uid" <"$clean" |
    python3 -c 'import json,sys; json.dump({"seal": sys.stdin.read().strip()}, open(sys.argv[1], "w"))' "$out"
  [[ -s $out ]]
}

# The `error` text of a JSON error body, or nothing.
almanac_error() {
  python3 -c 'import json,sys
try:
    d = json.load(open(sys.argv[1]))
    print(d.get("error") or "") if isinstance(d, dict) else None
except Exception:
    pass' "$1" 2>/dev/null
}
