//! Revisions, the changes feed, the trash, and If-Match (0.31.0).

mod common;

use almanac::http::Request;
use almanac::json::Value;
use common::*;

fn rev_of(v: &Value) -> i64 {
    match field(v, "rev") {
        Value::Number(n) => *n,
        other => panic!("rev is not a number: {other:?}"),
    }
}

fn uids(v: &Value, key: &str) -> Vec<String> {
    field(v, key)
        .as_array()
        .unwrap()
        .iter()
        .map(|i| text(i, "uid").to_string())
        .collect()
}

fn put_note(app: &almanac::AppState, cal: &Cal, uid: &str, title: &str, if_match: Option<&str>) -> (u16, Value) {
    let mut req = Request::new("PUT", &format!("/v1/c/{}/notes/{uid}", cal.id))
        .with_header("authorization", &format!("Bearer {}", cal.key))
        .with_header("content-type", "application/json")
        .with_body(format!(r#"{{"title":"{title}","body":"b"}}"#).into_bytes());
    if let Some(m) = if_match {
        req = req.with_header("if-match", m);
    }
    let (status, text, _) = send(app, req);
    (status, if text.is_empty() { Value::Null } else { json(&text) })
}

#[test]
fn every_write_gets_a_higher_rev() {
    let app = app();
    let cal = create(&app);
    let (_, a) = put_note(&app, &cal, "n1", "One", None);
    let (_, b) = put_note(&app, &cal, "n1", "One again", None);
    let (_, c) = call(&app, &cal.key, "POST", &format!("/v1/c/{}/todos", cal.id), Some(r#"{"title":"T"}"#));
    assert!(rev_of(&a) > 0);
    assert!(rev_of(&b) > rev_of(&a));
    assert!(rev_of(&c) > rev_of(&b), "one counter for the whole calendar");
}

#[test]
fn changes_since_a_cursor_returns_only_newer_items_and_deletes() {
    let app = app();
    let cal = create(&app);
    let base = format!("/v1/c/{}", cal.id);
    put_note(&app, &cal, "keep", "Keep", None);
    put_note(&app, &cal, "gone", "Gone", None);
    let (status, all) = call(&app, &cal.key, "GET", &format!("{base}/changes"), None);
    assert_eq!(status, 200);
    assert_eq!(uids(&all, "notes").len(), 2);
    let cursor = rev_of(&all);

    let (status, none) = call(&app, &cal.key, "GET", &format!("{base}/changes?since={cursor}"), None);
    assert_eq!(status, 200);
    assert!(uids(&none, "notes").is_empty());
    assert_eq!(rev_of(&none), cursor);

    put_note(&app, &cal, "keep", "Kept", None);
    assert_eq!(call(&app, &cal.key, "DELETE", &format!("{base}/notes/gone"), None).0, 204);
    let (_, delta) = call(&app, &cal.key, "GET", &format!("{base}/changes?since={cursor}"), None);
    assert_eq!(uids(&delta, "notes"), vec!["keep"]);
    let deleted = field(&delta, "deleted").as_array().unwrap();
    assert_eq!(deleted.len(), 1);
    assert_eq!(text(&deleted[0], "kind"), "note");
    assert_eq!(text(&deleted[0], "uid"), "gone");
    assert!(rev_of(&delta) > cursor);
}

#[test]
fn a_deleted_item_can_be_restored_from_the_trash() {
    let app = app();
    let cal = create(&app);
    let base = format!("/v1/c/{}", cal.id);
    let (_, todo) = call(&app, &cal.key, "POST", &format!("{base}/todos"), Some(r#"{"uid":"t1","title":"Milk"}"#));
    let before = rev_of(&todo);
    assert_eq!(call(&app, &cal.key, "DELETE", &format!("{base}/todos/t1"), None).0, 204);
    assert_eq!(call(&app, &cal.key, "GET", &format!("{base}/todos/t1"), None).0, 404);

    let (status, trash) = call(&app, &cal.key, "GET", &format!("{base}/trash"), None);
    assert_eq!(status, 200);
    let items = field(&trash, "items").as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(text(&items[0], "kind"), "todo");
    assert_eq!(text(&items[0], "title"), "Milk");

    let (status, back) = call(&app, &cal.key, "POST", &format!("{base}/todos/t1/restore"), None);
    assert_eq!(status, 200, "{back:?}");
    assert_eq!(text(&back, "title"), "Milk");
    assert!(rev_of(&back) > before);
    let (_, trash) = call(&app, &cal.key, "GET", &format!("{base}/trash"), None);
    assert!(field(&trash, "items").as_array().unwrap().is_empty());
    // Live again, so a second restore has nothing to bring back over it.
    assert_eq!(call(&app, &cal.key, "POST", &format!("{base}/todos/t1/restore"), None).0, 409);
    assert_eq!(call(&app, &cal.key, "POST", &format!("{base}/todos/never/restore"), None).0, 404);
}

#[test]
fn restoring_over_a_live_uid_is_refused() {
    let app = app();
    let cal = create(&app);
    let base = format!("/v1/c/{}", cal.id);
    put_note(&app, &cal, "n", "Old", None);
    call(&app, &cal.key, "DELETE", &format!("{base}/notes/n"), None);
    put_note(&app, &cal, "n", "New", None);
    // The new note took the uid, which also cleared the old one from the trash.
    assert_eq!(call(&app, &cal.key, "POST", &format!("{base}/notes/n/restore"), None).0, 409);
}

#[test]
fn a_restored_recurring_event_keeps_its_exceptions() {
    let app = app();
    let cal = create(&app);
    let base = format!("/v1/c/{}", cal.id);
    let (status, _) = call(
        &app,
        &cal.key,
        "PUT",
        &format!("{base}/events/standup"),
        Some(r#"{"summary":"Standup","start":"2026-11-02T09:00:00-08:00","end":"2026-11-02T09:15:00-08:00","timeZone":"America/Los_Angeles","rrule":"FREQ=DAILY;COUNT=5"}"#),
    );
    assert!(status == 200 || status == 201, "{status}");
    let (status, _) = call(
        &app,
        &cal.key,
        "PUT",
        &format!("{base}/events/standup/exdates"),
        Some(r#"{"recurrenceId":"2026-11-03T09:00:00"}"#),
    );
    assert_eq!(status, 201);
    let (_, before) = call(&app, &cal.key, "GET", &format!("{base}/events/standup"), None);
    assert_eq!(call(&app, &cal.key, "DELETE", &format!("{base}/events/standup"), None).0, 204);
    let (status, after) = call(&app, &cal.key, "POST", &format!("{base}/events/standup/restore"), None);
    assert_eq!(status, 200);
    assert_eq!(field(&after, "exdates"), field(&before, "exdates"));
}

#[test]
fn if_match_refuses_a_stale_write() {
    let app = app();
    let cal = create(&app);
    let (_, first) = put_note(&app, &cal, "n", "Draft", None);
    let seen = rev_of(&first).to_string();
    // Another device saves in between.
    put_note(&app, &cal, "n", "Theirs", None);
    let (status, conflict) = put_note(&app, &cal, "n", "Mine", Some(&format!("\"{seen}\"")));
    assert_eq!(status, 412);
    assert_eq!(field(&conflict, "deleted").as_bool(), Some(false));
    assert_eq!(text(field(&conflict, "current"), "title"), "Theirs");
    // With the current rev it goes through.
    let current = rev_of(field(&conflict, "current")).to_string();
    let (status, saved) = put_note(&app, &cal, "n", "Mine", Some(&current));
    assert_eq!(status, 200);
    assert_eq!(text(&saved, "title"), "Mine");
}

#[test]
fn if_match_on_a_deleted_item_says_so() {
    let app = app();
    let cal = create(&app);
    let base = format!("/v1/c/{}", cal.id);
    let (_, first) = put_note(&app, &cal, "n", "Draft", None);
    call(&app, &cal.key, "DELETE", &format!("{base}/notes/n"), None);
    let (status, conflict) = put_note(&app, &cal, "n", "Mine", Some(&rev_of(&first).to_string()));
    assert_eq!(status, 412);
    assert_eq!(field(&conflict, "deleted").as_bool(), Some(true));
}

#[test]
fn if_match_needs_auth_and_no_header_is_last_write_wins() {
    let app = app();
    let cal = create(&app);
    put_note(&app, &cal, "n", "A", None);
    let req = Request::new("DELETE", &format!("/v1/c/{}/notes/n", cal.id)).with_header("if-match", "1");
    assert_eq!(send(&app, req).0, 401);
    let (status, _) = put_note(&app, &cal, "n", "B", None);
    assert_eq!(status, 200);
}

#[test]
fn a_bad_cursor_is_400() {
    let app = app();
    let cal = create(&app);
    for bad in ["-1", "x"] {
        let (status, _) = call(&app, &cal.key, "GET", &format!("/v1/c/{}/changes?since={bad}", cal.id), None);
        assert_eq!(status, 400, "{bad}");
    }
}

#[test]
fn deleting_a_calendar_empties_its_trash() {
    let app = app();
    let cal = create(&app);
    put_note(&app, &cal, "n", "A", None);
    call(&app, &cal.key, "DELETE", &format!("/v1/c/{}/notes/n", cal.id), None);
    assert_eq!(call(&app, &cal.key, "DELETE", &format!("/v1/c/{}", cal.id), None).0, 204);
    let db = app.db.lock().unwrap();
    assert!(db.list_trash(&cal.id).unwrap().is_empty());
}

#[test]
fn purging_old_trash_expires_older_cursors() {
    let app = app();
    let cal = create(&app);
    let base = format!("/v1/c/{}", cal.id);
    let (_, n) = put_note(&app, &cal, "n", "A", None);
    let cursor = rev_of(&n);
    call(&app, &cal.key, "DELETE", &format!("{base}/notes/n"), None);
    // A cutoff in the future purges everything deleted so far.
    let removed = app.db.lock().unwrap().purge_trash("9999-01-01T00:00:00.000Z").unwrap();
    assert_eq!(removed, 1);
    let (status, _) = call(&app, &cal.key, "GET", &format!("{base}/changes?since={cursor}"), None);
    assert_eq!(status, 410);
    let (status, _) = call(&app, &cal.key, "GET", &format!("{base}/changes"), None);
    assert_eq!(status, 200, "a full sync always works");
}
