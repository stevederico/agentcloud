// Almanac client for browsers (and Node 20+). One ES module, no dependencies.
// Copied into each Dottie web app; this file in the almanac repo is the source.
//
// The write key never leaves the device and is never stored:
//   auth token = hex SHA-256(key + "almanac-auth-v1")   sent as the bearer
//   seal key   = SHA-256(key + "almanac-seal-v1")        AES-256-GCM, non-extractable
// The server only ever sees the token, which cannot open a seal.
//
// Seal wire format (matches almanac src/seal.rs):
//   "alm1." + base64url(nonce(12) || ciphertext || tag(16)), AAD = id \0 kind \0 uid

export const AUTH_DOMAIN = "almanac-auth-v1"
export const SEAL_DOMAIN = "almanac-seal-v1"
const PREFIX = "alm1."
const KINDS = { event: "events", todo: "todos", note: "notes" }
const SERVER_FIELDS = ["uid", "seal", "rev", "sequence", "createdAt", "updatedAt"]

const enc = new TextEncoder()
const dec = new TextDecoder()
const subtle = () => globalThis.crypto.subtle

function hex(buf) {
  return Array.from(new Uint8Array(buf), (b) => b.toString(16).padStart(2, "0")).join("")
}

export async function authToken(key) {
  return hex(await subtle().digest("SHA-256", enc.encode(String(key) + AUTH_DOMAIN)))
}

/** The hash the server stores for a token: what `POST /calendars {auth_hash}` takes. */
export async function authHash(token) {
  return hex(await subtle().digest("SHA-256", enc.encode(String(token))))
}

/** A non-extractable AES-GCM key: storable in IndexedDB, never readable as bytes. */
export async function sealKey(key) {
  const raw = await subtle().digest("SHA-256", enc.encode(String(key) + SEAL_DOMAIN))
  return subtle().importKey("raw", raw, { name: "AES-GCM" }, false, ["encrypt", "decrypt"])
}

/** A new random write key, 48 hex characters like the server makes. */
export function newKey() {
  return hex(globalThis.crypto.getRandomValues(new Uint8Array(24)))
}

function b64url(bytes) {
  let s = ""
  for (const b of bytes) s += String.fromCharCode(b)
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "")
}

function unb64url(text) {
  const clean = String(text).replace(/-/g, "+").replace(/_/g, "/").replace(/=+$/, "")
  if (!/^[A-Za-z0-9+/]*$/.test(clean) || clean.length % 4 === 1) throw new Error("seal is invalid")
  const bin = atob(clean + "===".slice((clean.length + 3) % 4))
  return Uint8Array.from(bin, (c) => c.charCodeAt(0))
}

function aad(calendarId, kind, uid) {
  return enc.encode(`${calendarId}\0${kind}\0${uid}`)
}

function checkKind(kind) {
  if (!KINDS[kind]) throw new Error("kind must be event, todo, or note")
}

export async function sealWith(cryptoKey, calendarId, kind, uid, plaintext) {
  checkKind(kind)
  if (!uid) throw new Error("uid is required")
  const plain = typeof plaintext === "string" ? enc.encode(plaintext) : plaintext
  if (plain.length > 48 * 1024) throw new Error("item is too long")
  const nonce = globalThis.crypto.getRandomValues(new Uint8Array(12))
  const ct = new Uint8Array(
    await subtle().encrypt({ name: "AES-GCM", iv: nonce, additionalData: aad(calendarId, kind, uid) }, cryptoKey, plain),
  )
  const packed = new Uint8Array(12 + ct.length)
  packed.set(nonce, 0)
  packed.set(ct, 12)
  const wire = PREFIX + b64url(packed)
  if (wire.length > 60000) throw new Error("seal is too long")
  return wire
}

export async function openWith(cryptoKey, calendarId, kind, uid, wire) {
  checkKind(kind)
  if (!String(wire || "").startsWith(PREFIX)) throw new Error("seal is invalid")
  const packed = unb64url(String(wire).slice(PREFIX.length))
  if (packed.length < 28) throw new Error("seal is invalid")
  try {
    const plain = await subtle().decrypt(
      { name: "AES-GCM", iv: packed.slice(0, 12), additionalData: aad(calendarId, kind, uid) },
      cryptoKey,
      packed.slice(12),
    )
    return dec.decode(plain)
  } catch {
    throw new Error("seal was rejected")
  }
}

/** Open a seal with the first key that fits; null keys are skipped. */
export async function openWithAny(keys, calendarId, kind, uid, wire) {
  for (const key of keys) {
    if (!key) continue
    try {
      return await openWith(key, calendarId, kind, uid, wire)
    } catch {}
  }
  throw new Error("seal was rejected")
}

/**
 * Open sealed rows (`{uid, seal}`) kept on this device, as after a sign-in
 * with a rotated key. Each is tried with the calendar's previous key (kept by
 * `store.keepPrevious` when the old key was rejected), then the session's.
 * Returns `{opened: [{row, fields}], unreadable: [row], previous}`. Seal the
 * opened fields again with the session's key and store them; only then call
 * `store.forgetPrevious(session.id)`. Keep the unreadable rows: never drop them.
 */
export async function openKept(store, session, kind, rows) {
  const previous = await store.previous(session.id)
  const out = { opened: [], unreadable: [], previous: !!previous }
  for (const row of rows || []) {
    try {
      const text = await openWithAny([previous, session.sealKey], session.id, kind, row.uid, row.seal)
      out.opened.push({ row, fields: JSON.parse(text) })
    } catch {
      out.unreadable.push(row)
    }
  }
  return out
}

export class AlmanacError extends Error {
  constructor(message, status, data) {
    super(message)
    this.status = status
    this.data = data
  }
  /** 412: the item changed (or was deleted) since it was read. */
  get conflict() {
    return this.status === 412
  }
}

export function messageFor(error) {
  if (!error) return "Almanac request failed"
  if (error.status === 401) return "That key does not open this calendar."
  if (error.status === 412) return "Changed on another device. Showing the latest."
  if (error.status === 429) return "Too many writes. Wait a moment."
  if (error.status === 0) return "Offline. Changes are kept on this device."
  return error.message ? String(error.message) : "Almanac request failed"
}

async function call(base, bearer, path, { method = "GET", body, ifMatch, fetchImpl } = {}) {
  const headers = { Accept: "application/json", Authorization: `Bearer ${bearer}` }
  if (body !== undefined) headers["Content-Type"] = "application/json"
  if (ifMatch !== undefined && ifMatch !== null) headers["If-Match"] = `"${ifMatch}"`
  let res
  try {
    res = await (fetchImpl || globalThis.fetch)(base + path, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      cache: "no-store",
    })
  } catch (e) {
    throw new AlmanacError("offline", 0, null)
  }
  const text = await res.text()
  let data = null
  if (text) {
    try {
      data = JSON.parse(text)
    } catch {
      data = { error: text.slice(0, 200) }
    }
  }
  if (!res.ok) {
    throw new AlmanacError(data && data.error ? String(data.error) : `Almanac ${res.status}`, res.status, data)
  }
  return data
}

/**
 * Sign in with a calendar id and its write key. Returns a session holding the
 * auth token and the non-extractable seal key; the key itself is dropped.
 * A calendar from before 0.30.0 learns its token on one raw-key request.
 */
export async function signIn({ id, key, base = "", fetchImpl } = {}) {
  id = String(id || "").trim()
  key = String(key || "").trim()
  if (!id || !key) throw new AlmanacError("Paste the calendar id and key.", 400, null)
  const token = await authToken(key)
  const path = `/v1/c/${encodeURIComponent(id)}`
  let info
  try {
    info = await call(base, token, path, { fetchImpl })
  } catch (e) {
    if (e.status !== 401) throw e
    await call(base, key, path, { fetchImpl })
    info = await call(base, token, path, { fetchImpl })
  }
  return {
    id: info.id || id,
    name: info.name || "Almanac",
    feed: info.feed || "plain",
    token,
    sealKey: await sealKey(key),
  }
}

/** API calls for one session. Items come back opened: sealed or not, the same fields. */
export function createClient(session, { base = "", fetchImpl } = {}) {
  const cal = `/v1/c/${encodeURIComponent(session.id)}`
  const req = (path, opts = {}) => call(base, session.token, cal + path, { ...opts, fetchImpl })
  const itemPath = (kind, uid) => `/${KINDS[kind]}/${encodeURIComponent(uid)}`

  async function openItem(kind, row) {
    if (!row || !row.seal) return row
    const fields = JSON.parse(await openWith(session.sealKey, session.id, kind, row.uid, row.seal))
    const out = { ...(fields && typeof fields === "object" ? fields : {}) }
    for (const k of SERVER_FIELDS) if (row[k] !== undefined) out[k] = row[k]
    delete out.seal
    out.sealed = true
    return out
  }

  async function openAll(kind, rows) {
    const out = []
    for (const row of rows || []) {
      try {
        out.push(await openItem(kind, row))
      } catch {
        out.push({ uid: row.uid, rev: row.rev, unreadable: true })
      }
    }
    return out
  }

  function plainFields(fields) {
    const out = { ...fields }
    for (const k of SERVER_FIELDS) delete out[k]
    delete out.sealed
    delete out.unreadable
    return out
  }

  const client = {
    session,
    async info() {
      const info = await req("")
      session.feed = info.feed || session.feed
      session.name = info.name || session.name
      return info
    },
    async list(kind) {
      checkKind(kind)
      const data = await req(`/${KINDS[kind]}${kind === "note" ? "?body=true" : ""}`)
      return openAll(kind, data[KINDS[kind]])
    },
    async get(kind, uid) {
      checkKind(kind)
      return openItem(kind, await req(itemPath(kind, uid)))
    },
    /**
     * Create or replace one item. `rev` (from the item you edited) makes the
     * write conditional: a 412 AlmanacError carries `data.current` (opened).
     * On a sealed calendar the fields are sealed here; on a plain one PUT keeps
     * fields you leave out, so it doubles as a patch (Cloudflare blocks PATCH).
     */
    async put(kind, uid, fields, { rev } = {}) {
      checkKind(kind)
      const body =
        session.feed === "seal"
          ? { seal: await sealWith(session.sealKey, session.id, kind, uid, JSON.stringify(plainFields(fields))) }
          : plainFields(fields)
      try {
        return await openItem(kind, await req(itemPath(kind, uid), { method: "PUT", body, ifMatch: rev }))
      } catch (e) {
        throw await withOpenedConflict(e, kind)
      }
    },
    /** Change some fields. Sealed items are opened, merged, and sealed again. */
    async update(kind, uid, changes, { rev } = {}) {
      if (session.feed !== "seal") return client.put(kind, uid, changes, { rev })
      const current = await client.get(kind, uid)
      if (rev !== undefined && rev !== null && current.rev !== rev) {
        throw new AlmanacError("changed elsewhere", 412, { deleted: false, current })
      }
      const merged = { ...plainFields(current), ...changes }
      for (const [k, v] of Object.entries(changes)) if (v === null) delete merged[k]
      return client.put(kind, uid, merged, { rev: current.rev })
    },
    async remove(kind, uid, { rev } = {}) {
      checkKind(kind)
      try {
        await req(itemPath(kind, uid), { method: "DELETE", ifMatch: rev })
      } catch (e) {
        if (e.status === 404) return
        throw await withOpenedConflict(e, kind)
      }
    },
    async restore(kind, uid) {
      checkKind(kind)
      return openItem(kind, await req(`${itemPath(kind, uid)}/restore`, { method: "POST" }))
    },
    async trash() {
      const data = await req("/trash")
      const out = []
      for (const item of data.items || []) {
        try {
          out.push({ ...(await openItem(item.kind, item)), kind: item.kind, deletedAt: item.deletedAt })
        } catch {
          out.push({ uid: item.uid, kind: item.kind, deletedAt: item.deletedAt, unreadable: true })
        }
      }
      return out
    },
    /** `{rev, events, todos, notes, deleted, full}`; `full` when the cursor was reset. */
    async changes(since = 0) {
      let data
      let full = !since
      try {
        data = await req(`/changes?since=${since || 0}`)
      } catch (e) {
        if (e.status !== 410) throw e
        data = await req("/changes?since=0")
        full = true
      }
      return {
        rev: data.rev,
        full,
        events: await openAll("event", data.events),
        todos: await openAll("todo", data.todos),
        notes: await openAll("note", data.notes),
        deleted: data.deleted || [],
      }
    },
  }

  async function withOpenedConflict(e, kind) {
    if (e.status === 412 && e.data && e.data.current) {
      try {
        e.data.current = await openItem(kind, e.data.current)
      } catch {}
    }
    return e
  }

  return client
}

/**
 * Poll `changes` on focus and every `intervalMs` while the page is visible.
 * `onChange(result)` gets each non-empty result; `result.full` means replace
 * everything (first run, or the cursor was older than the trash).
 */
export function createSync(client, { onChange, onError, intervalMs = 60000, cursor = 0 } = {}) {
  let timer = null
  let running = false
  let again = false
  const doc = globalThis.document

  async function now() {
    if (running) {
      again = true
      return
    }
    running = true
    try {
      do {
        again = false
        const result = await client.changes(cursor)
        const empty =
          !result.full &&
          !result.events.length &&
          !result.todos.length &&
          !result.notes.length &&
          !result.deleted.length
        // Move the cursor only after the handler took the batch, so a
        // handler that throws sees the same changes again next time.
        if (!empty && onChange) await onChange(result)
        cursor = result.rev
      } while (again)
    } catch (e) {
      if (onError) onError(e)
    } finally {
      running = false
    }
  }

  function visible() {
    return !doc || doc.visibilityState !== "hidden"
  }

  function schedule() {
    clearInterval(timer)
    timer = visible() ? setInterval(now, intervalMs) : null
  }

  function onVisibility() {
    schedule()
    if (visible()) now()
  }

  return {
    get cursor() {
      return cursor
    },
    now,
    start() {
      if (doc) doc.addEventListener("visibilitychange", onVisibility)
      if (globalThis.addEventListener) globalThis.addEventListener("focus", now)
      schedule()
      return now()
    },
    stop() {
      clearInterval(timer)
      timer = null
      if (doc) doc.removeEventListener("visibilitychange", onVisibility)
      if (globalThis.removeEventListener) globalThis.removeEventListener("focus", now)
    },
  }
}

/**
 * The session in IndexedDB: id, name, feed, token, and the seal key as a
 * non-extractable CryptoKey. Never the write key. Falls back to memory when
 * IndexedDB is unavailable. `clearLegacy(names)` removes old localStorage
 * entries that held the raw key. `keepPrevious(session)` keeps the seal key
 * as the calendar's previous key past `clear()`, for unsent items sealed with
 * it; `previous(id)` reads it, `forgetPrevious(id)` deletes it.
 */
export function sessionStore(name = "almanac") {
  let memory = null
  const previousKeys = new Map()
  const idb = globalThis.indexedDB

  function db() {
    return new Promise((resolve, reject) => {
      const open = idb.open(name, 1)
      open.onupgradeneeded = () => open.result.createObjectStore("session")
      open.onsuccess = () => resolve(open.result)
      open.onerror = () => reject(open.error)
    })
  }

  async function tx(mode, fn) {
    const handle = await db()
    return new Promise((resolve, reject) => {
      const t = handle.transaction("session", mode)
      const req = fn(t.objectStore("session"))
      t.oncomplete = () => {
        handle.close()
        resolve(req && req.result)
      }
      t.onerror = () => {
        handle.close()
        reject(t.error)
      }
    })
  }

  return {
    async save(session) {
      const row = {
        id: session.id,
        name: session.name,
        feed: session.feed,
        token: session.token,
        sealKey: session.sealKey,
      }
      memory = row
      if (!idb) return
      try {
        await tx("readwrite", (s) => s.put(row, "current"))
      } catch {}
    },
    async load() {
      if (!idb) return memory
      try {
        return (await tx("readonly", (s) => s.get("current"))) || memory
      } catch {
        return memory
      }
    },
    async clear() {
      memory = null
      if (!idb) return
      try {
        await tx("readwrite", (s) => s.delete("current"))
      } catch {}
    },
    async keepPrevious(session) {
      previousKeys.set(session.id, session.sealKey)
      if (!idb) return
      try {
        await tx("readwrite", (s) => s.put(session.sealKey, `previous:${session.id}`))
      } catch {}
    },
    async previous(id) {
      if (!idb) return previousKeys.get(id) || null
      try {
        return (await tx("readonly", (s) => s.get(`previous:${id}`))) || previousKeys.get(id) || null
      } catch {
        return previousKeys.get(id) || null
      }
    },
    async forgetPrevious(id) {
      previousKeys.delete(id)
      if (!idb) return
      try {
        await tx("readwrite", (s) => s.delete(`previous:${id}`))
      } catch {}
    },
    clearLegacy(keys) {
      for (const k of keys) {
        try {
          globalThis.localStorage && globalThis.localStorage.removeItem(k)
          globalThis.sessionStorage && globalThis.sessionStorage.removeItem(k)
        } catch {}
      }
    },
  }
}
