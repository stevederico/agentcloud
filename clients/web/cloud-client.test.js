// node --test clients/web/  (needs target/debug/agentcloud: cargo build)
import { test, before, after } from "node:test"
import assert from "node:assert/strict"
import { spawn, execFileSync } from "node:child_process"
import { createHash } from "node:crypto"
import { mkdtempSync, writeFileSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, dirname } from "node:path"
import { fileURLToPath } from "node:url"
import {
  authToken,
  authHash,
  sealKey,
  sealWith,
  openWith,
  openWithAny,
  openKept,
  sessionStore,
  newKey,
  signIn,
  createClient,
  createSync,
  CloudError,
} from "./cloud-client.js"

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..")
const BIN = process.env.AGENTCLOUD_BIN || process.env.ALMANAC_BIN || join(root, "target", "debug", "agentcloud")
const UA = { "User-Agent": "agentcloud-client-test" }
let server
let base
let dir

before(async () => {
  dir = mkdtempSync(join(tmpdir(), "agentcloud-client-"))
  const port = 30000 + Math.floor(Math.random() * 20000)
  base = `http://127.0.0.1:${port}`
  server = spawn(BIN, [], {
    env: { ...process.env, PORT: String(port), HOST: "127.0.0.1", DB_PATH: join(dir, "c.db"), AGENT_KEY: "h", FEED_TOKEN: "f", BACKUP_INTERVAL_HOURS: "0" },
    stdio: "ignore",
  })
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`${base}/health`)).ok) return
    } catch {}
    await new Promise((r) => setTimeout(r, 50))
  }
  throw new Error("server did not start")
})

after(() => {
  server.kill()
  rmSync(dir, { recursive: true, force: true })
})

async function legacyCalendar(feed) {
  const res = await fetch(`${base}/calendars`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json", ...UA },
    body: JSON.stringify({ name: "T", feed }),
  })
  assert.equal(res.status, 201)
  return res.json()
}

async function ownCalendar(feed) {
  const key = newKey()
  const token = await authToken(key)
  const res = await fetch(`${base}/calendars`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json", ...UA },
    body: JSON.stringify({ name: "Mine", feed, auth_hash: await authHash(token) }),
  })
  assert.equal(res.status, 201)
  const body = await res.json()
  assert.equal(body.key, undefined, "the server never had the key")
  return { id: body.id, key }
}

test("the auth token matches the server's derivation", async () => {
  const want = createHash("sha256").update("abcalmanac-auth-v1").digest("hex")
  assert.equal(await authToken("abc"), want)
})

test("seals interoperate with the agentcloud binary", async () => {
  const key = newKey()
  const creds = join(dir, "creds.json")
  writeFileSync(creds, JSON.stringify([{ id: "cal_x", key }]), { mode: 0o600 })
  const env = { ...process.env, AGENTCLOUD_CREDS: creds }
  const wire = await sealWith(await sealKey(key), "cal_x", "note", "n1", '{"title":"hi"}')
  const opened = execFileSync(BIN, ["open", "--kind", "note", "--uid", "n1"], { input: wire, env }).toString()
  assert.equal(opened.trim(), '{"title":"hi"}')
  const back = execFileSync(BIN, ["seal", "--kind", "todo", "--uid", "t1"], { input: '{"title":"x"}', env }).toString().trim()
  assert.equal(await openWith(await sealKey(key), "cal_x", "todo", "t1", back), '{"title":"x"}')
  await assert.rejects(openWith(await sealKey(key), "cal_x", "todo", "t2", back), /rejected/)
})

test("sign-in upgrades a legacy calendar and keeps no key", async () => {
  const cal = await legacyCalendar("plain")
  const session = await signIn({ id: cal.id, key: cal.key, base })
  assert.equal(session.key, undefined)
  assert.equal(session.token, await authToken(cal.key))
  assert.equal(session.sealKey.extractable, false)
  await assert.rejects(signIn({ id: cal.id, key: "wrong", base }), (e) => e.status === 401)
})

test("a sealed calendar round-trips and the server only sees ciphertext", async () => {
  const { id, key } = await ownCalendar("seal")
  const session = await signIn({ id, key, base })
  assert.equal(session.feed, "seal")
  const client = createClient(session, { base })
  const saved = await client.put("note", "n1", { title: "Secret", body: "inside" })
  assert.equal(saved.title, "Secret")
  assert.equal(saved.sealed, true)
  const raw = await (await fetch(`${base}/v1/c/${id}/notes/n1`, { headers: { Authorization: `Bearer ${session.token}`, ...UA } })).json()
  assert.ok(raw.seal.startsWith("alm1."))
  assert.equal(raw.title, undefined)
  const updated = await client.update("note", "n1", { body: "changed" }, { rev: saved.rev })
  assert.equal(updated.title, "Secret")
  assert.equal(updated.body, "changed")
  const list = await client.list("note")
  assert.deepEqual(list.map((n) => n.title), ["Secret"])
})

test("changes, conflicts, delete and restore", async () => {
  const cal = await legacyCalendar("plain")
  const client = createClient(await signIn({ id: cal.id, key: cal.key, base }), { base })
  const first = await client.changes(0)
  assert.equal(first.full, true)
  const a = await client.put("todo", "milk", { title: "Milk" })
  const b = await client.put("todo", "milk", { title: "Oat milk" })
  await assert.rejects(client.put("todo", "milk", { title: "Mine" }, { rev: a.rev }), (e) => {
    assert.ok(e instanceof CloudError)
    assert.equal(e.conflict, true)
    assert.equal(e.data.current.title, "Oat milk")
    return true
  })
  const delta = await client.changes(first.rev)
  assert.deepEqual(delta.todos.map((t) => t.title), ["Oat milk"])
  await client.remove("todo", "milk", { rev: b.rev })
  const gone = await client.changes(delta.rev)
  assert.deepEqual(gone.deleted.map((d) => d.uid), ["milk"])
  assert.equal((await client.trash())[0].title, "Oat milk")
  const back = await client.restore("todo", "milk")
  assert.equal(back.title, "Oat milk")
})

test("sync reports a full load first, then only changes", async () => {
  const cal = await legacyCalendar("plain")
  const client = createClient(await signIn({ id: cal.id, key: cal.key, base }), { base })
  await client.put("note", "n", { title: "One" })
  const seen = []
  const sync = createSync(client, { onChange: (r) => seen.push(r), intervalMs: 1e9 })
  await sync.start()
  await sync.now()
  await client.put("note", "n", { title: "Two" })
  await sync.now()
  sync.stop()
  assert.equal(seen.length, 2, "the empty poll in between is not reported")
  assert.equal(seen[0].full, true)
  assert.equal(seen[1].full, false)
  assert.deepEqual(seen[1].notes.map((n) => n.title), ["Two"])
})

test("unsent seals from before a key rotation open with the kept previous key", async () => {
  const store = sessionStore("agentcloud-test")
  const old = { id: "cal_r", feed: "seal", sealKey: await sealKey(newKey()) }
  const now = { id: "cal_r", feed: "seal", sealKey: await sealKey(newKey()) }
  const rows = [
    { uid: "a", seal: await sealWith(old.sealKey, "cal_r", "todo", "a", '{"title":"Before"}') },
    { uid: "b", seal: await sealWith(now.sealKey, "cal_r", "todo", "b", '{"title":"After"}') },
    { uid: "c", seal: await sealWith(await sealKey(newKey()), "cal_r", "todo", "c", '{"title":"Other"}') },
  ]
  await store.save(old)
  await store.keepPrevious(old)
  await store.clear()
  const previous = await store.previous("cal_r")
  assert.equal(previous, old.sealKey, "the session ending keeps the previous key")
  assert.equal(previous.extractable, false)
  assert.equal(await store.previous("cal_other"), null)
  const kept = await openKept(store, now, "todo", rows)
  assert.equal(kept.previous, true)
  assert.deepEqual(kept.opened.map((o) => `${o.row.uid}:${o.fields.title}`), ["a:Before", "b:After"])
  assert.deepEqual(kept.unreadable.map((row) => row.uid), ["c"], "neither key: kept, not dropped")
  await store.forgetPrevious("cal_r")
  assert.equal(await store.previous("cal_r"), null)
  const later = await openKept(store, now, "todo", rows)
  assert.equal(later.previous, false)
  assert.deepEqual(later.unreadable.map((row) => row.uid), ["a", "c"])
  await assert.rejects(openWithAny([null, now.sealKey], "cal_r", "todo", "a", rows[0].seal), /rejected/)
})
