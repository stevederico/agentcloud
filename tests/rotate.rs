//! Key rotation (0.32.0): begin with the new token's hash, commit with the new token.

mod common;

use almanac::seal::{auth_token, open, seal};
use almanac::sha256::sha256_hex;
use common::*;

const NEW_KEY: &str = "rotated-key-for-tests-0000";

fn begin(app: &almanac::AppState, cal: &Cal, token: &str) -> u16 {
    let body = format!(r#"{{"auth_hash":"{}"}}"#, sha256_hex(&auth_token(NEW_KEY)));
    call(app, token, "POST", &format!("/v1/c/{}/rotate", cal.id), Some(&body)).0
}

#[test]
fn a_rotation_swaps_the_key_and_can_change_the_feeds() {
    let app = app();
    let cal = create(&app);
    let old = auth_token(&cal.key);
    let new = auth_token(NEW_KEY);
    let info = format!("/v1/c/{}", cal.id);
    assert_eq!(begin(&app, &cal, &old), 200);
    // Both work while it is open.
    assert_eq!(call(&app, &old, "GET", &info, None).0, 200);
    assert_eq!(call(&app, &new, "GET", &info, None).0, 200);
    // Commit needs the new token.
    assert_eq!(call(&app, &old, "POST", &format!("{info}/rotate/commit"), None).0, 409);
    let (status, view) = call(&app, &new, "POST", &format!("{info}/rotate/commit"), Some(r#"{"feeds":true}"#));
    assert_eq!(status, 200, "{view:?}");
    assert_eq!(field(&view, "legacyAuth").as_bool(), Some(false));
    assert_eq!(call(&app, &old, "GET", &info, None).0, 401);
    assert_eq!(call(&app, &cal.key, "GET", &info, None).0, 401);
    assert_eq!(call(&app, &new, "GET", &info, None).0, 200);
    // Old feed URLs are dead, new ones serve.
    let (status, _, _) = send(&app, almanac::http::Request::new("GET", &cal.events_feed_path));
    assert_eq!(status, 404);
    let fresh = text(&view, "subscribe").strip_prefix(BASE).unwrap().to_string();
    assert_ne!(fresh, cal.events_feed_path);
    let (status, _, _) = send(&app, almanac::http::Request::new("GET", &fresh));
    assert_eq!(status, 200);
}

#[test]
fn without_feeds_the_subscribe_urls_stay() {
    let app = app();
    let cal = create(&app);
    let info = format!("/v1/c/{}", cal.id);
    begin(&app, &cal, &cal.key);
    let (status, view) = call(&app, &auth_token(NEW_KEY), "POST", &format!("{info}/rotate/commit"), None);
    assert_eq!(status, 200);
    assert_eq!(text(&view, "subscribe").strip_prefix(BASE).unwrap(), cal.events_feed_path);
}

#[test]
fn a_cancelled_rotation_leaves_the_old_key() {
    let app = app();
    let cal = create(&app);
    let old = auth_token(&cal.key);
    let info = format!("/v1/c/{}", cal.id);
    begin(&app, &cal, &old);
    assert_eq!(call(&app, &old, "POST", &format!("{info}/rotate/cancel"), None).0, 204);
    assert_eq!(call(&app, &auth_token(NEW_KEY), "GET", &info, None).0, 401);
    assert_eq!(call(&app, &old, "GET", &info, None).0, 200);
    assert_eq!(call(&app, &old, "POST", &format!("{info}/rotate/commit"), None).0, 409);
}

#[test]
fn resealed_items_open_with_the_new_key_and_sealed_trash_is_dropped() {
    let app = app();
    let cal = create(&app);
    let info = format!("/v1/c/{}", cal.id);
    call(&app, &cal.key, "PATCH", &info, Some(r#"{"feed":"seal"}"#));
    let put = |key: &str, uid: &str| {
        let wire = seal(key, &cal.id, "note", uid, br#"{"title":"secret"}"#).unwrap();
        let body = format!(r#"{{"seal":"{wire}"}}"#);
        call(&app, &auth_token(key), "PUT", &format!("{info}/notes/{uid}"), Some(&body)).0
    };
    assert!(matches!(put(&cal.key, "n1"), 200 | 201));
    assert!(matches!(put(&cal.key, "n2"), 200 | 201));
    call(&app, &cal.key, "DELETE", &format!("{info}/notes/n2"), None);

    begin(&app, &cal, &cal.key);
    assert!(matches!(put(NEW_KEY, "n1"), 200 | 201));
    let new = auth_token(NEW_KEY);
    assert_eq!(call(&app, &new, "POST", &format!("{info}/rotate/commit"), None).0, 200);

    let (_, note) = call(&app, &new, "GET", &format!("{info}/notes/n1"), None);
    let wire = text(&note, "seal");
    assert!(open(NEW_KEY, &cal.id, "note", "n1", wire).is_ok());
    assert!(open(&cal.key, &cal.id, "note", "n1", wire).is_err());
    let (_, trash) = call(&app, &new, "GET", &format!("{info}/trash"), None);
    assert!(field(&trash, "items").as_array().unwrap().is_empty());
}

#[test]
fn home_cannot_rotate_and_bad_input_is_refused() {
    let app = app();
    assert_eq!(
        call(&app, HOME_KEY, "POST", "/v1/c/home/rotate", Some(r#"{"auth_hash":"00"}"#)).0,
        403
    );
    let cal = create(&app);
    let info = format!("/v1/c/{}", cal.id);
    assert_eq!(call(&app, &cal.key, "POST", &format!("{info}/rotate"), Some("{}")).0, 400);
    assert_eq!(
        call(&app, &cal.key, "POST", &format!("{info}/rotate"), Some(r#"{"auth_hash":"xyz"}"#)).0,
        400
    );
    assert_eq!(call(&app, &cal.key, "GET", &format!("{info}/rotate"), None).0, 404);
}
