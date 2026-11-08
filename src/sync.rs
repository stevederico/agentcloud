//! Sync revisions, a 30-day trash, and the changes feed (0.31.0).
//!
//! Every calendar has a `rev` counter. SQLite triggers bump it on each insert,
//! update or delete of an event, todo or note, and stamp the row with the new
//! value. A delete moves the row into `trash_<table>` with the rev of the
//! delete, so a client that synced at rev N can learn about it, and the owner
//! can undo it. Rows stay in the trash for `TRASH_DAYS`; purging records
//! `purged_rev`, and a client whose cursor is older must do a full resync.
//!
//! Triggers are rebuilt on every open from the live column list, so a later
//! `ALTER TABLE ... ADD COLUMN` is carried into the trash without a new
//! migration here.

use crate::db::{column_names, json_event, table_names, Db, EventRow};
use crate::json::Value;
use crate::notes::{json_note, NoteRow};
use crate::sqlite::{Bind, Connection};
use crate::todos::{json_todo, TodoRow};

pub const TRASH_DAYS: i64 = 30;

/// Item tables and the API name of their kind.
pub const KINDS: [(&str, &str); 3] = [("events", "event"), ("todos", "todo"), ("notes", "note")];

pub fn table_for(kind: &str) -> Option<&'static str> {
    KINDS.iter().find(|(_, k)| *k == kind).map(|(t, _)| *t)
}

pub(crate) fn migrate(db: &Connection) -> Result<(), String> {
    let tx = db.begin()?;
    let cal_cols = column_names(db, "calendars")?;
    for col in ["rev", "purged_rev"] {
        if !cal_cols.iter().any(|c| c == col) {
            db.execute_batch(&format!(
                "ALTER TABLE calendars ADD COLUMN {col} INTEGER NOT NULL DEFAULT 0;"
            ))?;
        }
    }
    let tables = table_names(db)?;
    for (table, _) in KINDS {
        if !column_names(db, table)?.iter().any(|c| c == "rev") {
            db.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN rev INTEGER NOT NULL DEFAULT 0;"
            ))?;
        }
        let trash = format!("trash_{table}");
        sync_trash_table(db, table, &trash, &tables)?;
        db.execute_batch(&format!(
            "CREATE UNIQUE INDEX IF NOT EXISTS {trash}_key ON {trash}(calendar_id, uid);
             CREATE INDEX IF NOT EXISTS {trash}_rev ON {trash}(calendar_id, trash_rev);
             CREATE INDEX IF NOT EXISTS idx_{table}_rev ON {table}(calendar_id, rev);"
        ))?;
        install_triggers(db, table, &trash)?;
    }
    sync_trash_table(db, "event_exceptions", "trash_event_exceptions", &tables)?;
    tx.commit()
}

/// Create `trash` with `table`'s columns, or add any it is missing.
fn sync_trash_table(db: &Connection, table: &str, trash: &str, tables: &[String]) -> Result<(), String> {
    if !tables.iter().any(|t| t == trash) {
        db.execute_batch(&format!(
            "CREATE TABLE {trash} AS SELECT * FROM {table} WHERE 0;
             ALTER TABLE {trash} ADD COLUMN deleted_at TEXT NOT NULL DEFAULT '';
             ALTER TABLE {trash} ADD COLUMN trash_rev INTEGER NOT NULL DEFAULT 0;"
        ))?;
        return Ok(());
    }
    let have = column_names(db, trash)?;
    for col in column_names(db, table)? {
        if !have.contains(&col) {
            db.execute_batch(&format!("ALTER TABLE {trash} ADD COLUMN {col} DEFAULT '';"))?;
        }
    }
    Ok(())
}

fn install_triggers(db: &Connection, table: &str, trash: &str) -> Result<(), String> {
    let cols = column_names(db, table)?;
    let list = cols.join(", ");
    let old = cols.iter().map(|c| format!("OLD.{c}")).collect::<Vec<_>>().join(", ");
    // A new event under a deleted uid must not inherit the old one's
    // exceptions on a later restore.
    let clear_exceptions = if table == "events" {
        "DELETE FROM trash_event_exceptions WHERE calendar_id = NEW.calendar_id AND uid = NEW.uid;"
    } else {
        ""
    };
    // `WHEN NEW.rev IS OLD.rev`: the trigger's own rev stamp is an update too,
    // and must not count as another change.
    db.execute_batch(&format!(
        "DROP TRIGGER IF EXISTS {table}_rev_insert;
         DROP TRIGGER IF EXISTS {table}_rev_update;
         DROP TRIGGER IF EXISTS {table}_trash;
         CREATE TRIGGER {table}_rev_insert AFTER INSERT ON {table} BEGIN
           UPDATE calendars SET rev = rev + 1 WHERE id = NEW.calendar_id;
           UPDATE {table} SET rev = (SELECT rev FROM calendars WHERE id = NEW.calendar_id)
             WHERE calendar_id = NEW.calendar_id AND uid = NEW.uid;
           DELETE FROM {trash} WHERE calendar_id = NEW.calendar_id AND uid = NEW.uid;
           {clear_exceptions}
         END;
         CREATE TRIGGER {table}_rev_update AFTER UPDATE ON {table} WHEN NEW.rev IS OLD.rev BEGIN
           UPDATE calendars SET rev = rev + 1 WHERE id = NEW.calendar_id;
           UPDATE {table} SET rev = (SELECT rev FROM calendars WHERE id = NEW.calendar_id)
             WHERE calendar_id = NEW.calendar_id AND uid = NEW.uid;
         END;
         CREATE TRIGGER {table}_trash AFTER DELETE ON {table} BEGIN
           UPDATE calendars SET rev = rev + 1 WHERE id = OLD.calendar_id;
           INSERT OR REPLACE INTO {trash} ({list}, deleted_at, trash_rev)
             VALUES ({old}, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                     (SELECT rev FROM calendars WHERE id = OLD.calendar_id));
         END;"
    ))
}

/// What changed on a calendar after `since`.
pub struct Changes {
    pub rev: i64,
    pub events: Vec<EventRow>,
    pub todos: Vec<TodoRow>,
    pub notes: Vec<NoteRow>,
    /// (kind, uid, rev of the delete)
    pub deleted: Vec<(String, String, i64)>,
}

#[derive(Debug, PartialEq)]
pub enum ChangesError {
    /// The cursor is older than the oldest delete still on record.
    Expired,
    Db(String),
}

impl From<String> for ChangesError {
    fn from(e: String) -> Self {
        ChangesError::Db(e)
    }
}

impl Db {
    /// Row counts for the restore test and `almanac stats`. No contents.
    pub fn stats(&self) -> Result<Vec<(&'static str, i64)>, String> {
        let mut out = Vec::new();
        for table in ["calendars", "events", "todos", "notes", "trash_events", "trash_todos", "trash_notes"] {
            let n = self
                .conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), &[], |row| row.i64(0))?
                .unwrap_or(0);
            out.push((table, n));
        }
        Ok(out)
    }

    pub fn calendar_revs(&self, calendar_id: &str) -> Result<(i64, i64), String> {
        Ok(self
            .conn
            .query_row(
                "SELECT rev, purged_rev FROM calendars WHERE id = ?1",
                &[Bind::Text(calendar_id)],
                |row| (row.i64(0), row.i64(1)),
            )?
            .unwrap_or((0, 0)))
    }

    /// `since` 0 is a full snapshot (rows from before 0.31.0 have rev 0).
    pub fn changes(&self, calendar_id: &str, since: i64) -> Result<Changes, ChangesError> {
        let (rev, purged) = self.calendar_revs(calendar_id)?;
        // A cursor past the counter comes from before a restore from backup:
        // the revs after the backup are being handed out again, so a delta
        // would skip them. Make the client start over.
        if since > 0 && (since < purged || since > rev) {
            return Err(ChangesError::Expired);
        }
        if since == 0 {
            return Ok(Changes {
                rev,
                events: self.list_events(calendar_id)?,
                todos: self.list_todos(calendar_id, None)?,
                notes: self.list_notes(calendar_id, None, 0)?,
                deleted: Vec::new(),
            });
        }
        // Only the rows after the cursor, through the (calendar_id, rev) index.
        let binds = [Bind::Text(calendar_id), Bind::I64(since)];
        let uids = self.conn.query(
            "SELECT uid FROM events WHERE calendar_id = ?1 AND rev > ?2 ORDER BY rev",
            &binds,
            |row| row.text(0),
        )?;
        let mut events = Vec::with_capacity(uids.len());
        for uid in uids {
            // get_event attaches the exdates and overrides.
            if let Some(event) = self.get_event(calendar_id, &uid)? {
                events.push(event);
            }
        }
        let todos = self.conn.query(
            &format!(
                "SELECT {} FROM todos WHERE calendar_id = ?1 AND rev > ?2 ORDER BY rev",
                crate::todos::TODO_COLS
            ),
            &binds,
            crate::todos::row_todo,
        )?;
        let notes = self.conn.query(
            &format!(
                "SELECT {} FROM notes WHERE calendar_id = ?1 AND rev > ?2 ORDER BY rev",
                crate::notes::NOTE_COLS
            ),
            &binds,
            crate::notes::row_note,
        )?;
        let mut deleted = Vec::new();
        {
            for (table, kind) in KINDS {
                let rows = self.conn.query(
                    &format!(
                        "SELECT uid, trash_rev FROM trash_{table}
                         WHERE calendar_id = ?1 AND trash_rev > ?2 ORDER BY trash_rev"
                    ),
                    &[Bind::Text(calendar_id), Bind::I64(since)],
                    |row| (kind.to_string(), row.text(0), row.i64(1)),
                )?;
                deleted.extend(rows);
            }
        }
        Ok(Changes { rev, events, todos, notes, deleted })
    }

    /// Current rev of one live item, or `None` if it does not exist.
    pub fn item_rev(&self, kind: &str, calendar_id: &str, uid: &str) -> Result<Option<i64>, String> {
        let table = table_for(kind).ok_or("unknown kind")?;
        self.conn.query_row(
            &format!("SELECT rev FROM {table} WHERE calendar_id = ?1 AND uid = ?2"),
            &[Bind::Text(calendar_id), Bind::Text(uid)],
            |row| row.i64(0),
        )
    }

    /// Put an event's exceptions in the trash next to it, so a restore brings
    /// back its skipped and moved occurrences. Call before deleting them.
    pub(crate) fn trash_exceptions(&self, calendar_id: &str, uid: &str) -> Result<(), String> {
        let cols = column_names(&self.conn, "event_exceptions")?.join(", ");
        let binds = [Bind::Text(calendar_id), Bind::Text(uid)];
        self.conn.execute(
            "DELETE FROM trash_event_exceptions WHERE calendar_id = ?1 AND uid = ?2",
            &binds,
        )?;
        self.conn.execute(
            &format!(
                "INSERT INTO trash_event_exceptions ({cols}) SELECT {cols} FROM event_exceptions
                 WHERE calendar_id = ?1 AND uid = ?2"
            ),
            &binds,
        )?;
        Ok(())
    }

    /// Move an item back out of the trash. `Ok(false)` if it is not there.
    /// `Err("exists")` if a live item already has that uid.
    pub fn restore_item(&self, kind: &str, calendar_id: &str, uid: &str) -> Result<bool, String> {
        let table = table_for(kind).ok_or("unknown kind")?;
        let tx = self.conn.begin()?;
        let binds = [Bind::Text(calendar_id), Bind::Text(uid)];
        if self.item_rev(kind, calendar_id, uid)?.is_some() {
            return Err("exists".into());
        }
        if table == "events" {
            // Before the event: its insert trigger clears trashed exceptions.
            let ex = column_names(&self.conn, "event_exceptions")?.join(", ");
            self.conn.execute(
                &format!(
                    "INSERT INTO event_exceptions ({ex}) SELECT {ex} FROM trash_event_exceptions
                     WHERE calendar_id = ?1 AND uid = ?2"
                ),
                &binds,
            )?;
        }
        let cols = column_names(&self.conn, table)?.join(", ");
        let moved = self.conn.execute(
            &format!(
                "INSERT INTO {table} ({cols}) SELECT {cols} FROM trash_{table}
                 WHERE calendar_id = ?1 AND uid = ?2"
            ),
            &binds,
        )?;
        if moved == 0 {
            return Ok(false);
        }
        tx.commit()?;
        Ok(true)
    }

    /// Items in the trash, newest delete first, each with `kind`, `deletedAt`
    /// and `deletedRev`.
    pub fn list_trash(&self, calendar_id: &str) -> Result<Vec<Value>, String> {
        let mut out = Vec::new();
        for (table, kind) in KINDS {
            let cols = match table {
                "events" => crate::db::EVENT_COLS,
                "todos" => crate::todos::TODO_COLS,
                _ => crate::notes::NOTE_COLS,
            };
            // `deleted_at` and `trash_rev` follow the item columns.
            let n = cols.split(',').count() as i32;
            let rows = self.conn.query(
                &format!(
                    "SELECT {cols}, deleted_at, trash_rev FROM trash_{table}
                     WHERE calendar_id = ?1 ORDER BY trash_rev DESC"
                ),
                &[Bind::Text(calendar_id)],
                |row| {
                    let mut item = match table {
                        "events" => json_event(&crate::db::row_event(row)),
                        "todos" => json_todo(&crate::todos::row_todo(row)),
                        _ => json_note(&crate::notes::row_note(row), false),
                    };
                    if let Value::Object(map) = &mut item {
                        map.insert("kind".into(), Value::String(kind.into()));
                        map.insert("deletedAt".into(), Value::String(row.text(n)));
                        map.insert("deletedRev".into(), Value::Number(row.i64(n + 1)));
                    }
                    (row.i64(n + 1), item)
                },
            )?;
            out.extend(rows);
        }
        out.sort_by_key(|item| std::cmp::Reverse(item.0));
        Ok(out.into_iter().map(|(_, v)| v).collect())
    }

    /// Drop trash older than `cutoff` (an ISO instant) and raise each
    /// calendar's `purged_rev` past what was dropped. Returns rows removed.
    pub fn purge_trash(&self, cutoff: &str) -> Result<usize, String> {
        let tx = self.conn.begin()?;
        let mut removed = 0;
        for (table, _) in KINDS {
            self.conn.execute(
                &format!(
                    "UPDATE calendars SET purged_rev = MAX(purged_rev, (
                       SELECT MAX(trash_rev) FROM trash_{table}
                       WHERE calendar_id = calendars.id AND deleted_at < ?1))
                     WHERE EXISTS (SELECT 1 FROM trash_{table}
                       WHERE calendar_id = calendars.id AND deleted_at < ?1)"
                ),
                &[Bind::Text(cutoff)],
            )?;
            removed += self.conn.execute(
                &format!("DELETE FROM trash_{table} WHERE deleted_at < ?1"),
                &[Bind::Text(cutoff)],
            )?;
        }
        self.conn.execute(
            "DELETE FROM trash_event_exceptions WHERE NOT EXISTS (
               SELECT 1 FROM trash_events t
               WHERE t.calendar_id = trash_event_exceptions.calendar_id
                 AND t.uid = trash_event_exceptions.uid)",
            &[],
        )?;
        tx.commit()?;
        Ok(removed)
    }
}

/// `{"rev":N,"events":[..],"todos":[..],"notes":[..],"deleted":[{kind,uid,rev}]}`.
/// Note bodies are included: a sync client needs them.
pub fn json_changes(c: &Changes) -> Value {
    Value::object(&[
        ("rev", Value::Number(c.rev)),
        ("events", Value::Array(c.events.iter().map(json_event).collect())),
        ("todos", Value::Array(c.todos.iter().map(json_todo).collect())),
        (
            "notes",
            Value::Array(c.notes.iter().map(|n| json_note(n, true)).collect()),
        ),
        (
            "deleted",
            Value::Array(
                c.deleted
                    .iter()
                    .map(|(kind, uid, rev)| {
                        Value::object(&[
                            ("kind", Value::String(kind.clone())),
                            ("uid", Value::String(uid.clone())),
                            ("rev", Value::Number(*rev)),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}
