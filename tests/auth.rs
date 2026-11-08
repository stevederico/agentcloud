//! The auth token split from the write key (0.30.0).

mod common;

use agentcloud::http::Request;
use agentcloud::seal::{auth_token, open_file, seal_file};
use agentcloud::sha256::sha256_hex;
use common::*;

#[test]
fn the_auth_token_is_sha256_of_key_and_domain() {
    // Fixed vector: clients (WebCrypto, shell) must derive the same token.
    assert_eq!(
        auth_token("abc"),
        sha256_hex("abcalmanac-auth-v1"),
    );
    assert_eq!(auth_token("abc").len(), 64);
    assert_ne!(auth_token("abc"), auth_token("abd"));
}

#[test]
fn the_raw_key_and_the_auth_token_both_work_while_legacy_is_on() {
    let app = app();
    let cal = create(&app);
    let path = format!("/v1/c/{}", cal.id);
    let (status, info) = call(&app, &cal.key, "GET", &path, None);
    assert_eq!(status, 200);
    assert_eq!(field(&info, "legacyAuth").as_bool(), Some(true));
    let (status, _) = call(&app, &auth_token(&cal.key), "GET", &path, None);
    assert_eq!(status, 200);
    let (status, _) = call(&app, "wrong", "GET", &path, None);
    assert_eq!(status, 401);
    let (status, _) = call(&app, "", "GET", &path, None);
    assert_eq!(status, 401);
}

#[test]
fn turning_legacy_off_rejects_the_raw_key() {
    let app = app();
    let cal = create(&app);
    let token = auth_token(&cal.key);
    let path = format!("/v1/c/{}", cal.id);
    let (status, info) = call(&app, &token, "PATCH", &path, Some(r#"{"legacy_auth":false}"#));
    assert_eq!(status, 200, "{info:?}");
    assert_eq!(field(&info, "legacyAuth").as_bool(), Some(false));
    assert_eq!(call(&app, &cal.key, "GET", &path, None).0, 401);
    assert_eq!(call(&app, &token, "GET", &path, None).0, 200);
    // POST is the same call (Cloudflare blocks PATCH in front of prod).
    let (status, _) = call(&app, &token, "POST", &path, Some(r#"{"feed":"plain"}"#));
    assert_eq!(status, 200);
    // There is no way back: the raw key's hash is gone.
    let (status, _) = call(&app, &token, "PATCH", &path, Some(r#"{"legacy_auth":true}"#));
    assert_eq!(status, 400);
}

#[test]
fn the_home_calendar_keeps_its_environment_key() {
    let app = app();
    let (status, _) = call(&app, HOME_KEY, "PATCH", "/v1/c/home", Some(r#"{"legacy_auth":false}"#));
    assert_eq!(status, 403);
    assert_eq!(call(&app, &auth_token(HOME_KEY), "GET", "/v1/c/home", None).0, 200);
}

#[test]
fn a_calendar_made_from_an_auth_hash_never_sees_the_key() {
    let app = app();
    let key = "client-made-key-0123456789abcdef0123456789abcdef";
    let token = auth_token(key);
    let body = format!(r#"{{"name":"Mine","feed":"seal","auth_hash":"{}"}}"#, sha256_hex(&token));
    let (status, text, _) = send(
        &app,
        Request::new("POST", "/calendars")
            .with_header("content-type", "application/json")
            .with_header("accept", "application/json")
            .with_body(body.into_bytes()),
    );
    assert_eq!(status, 201, "{text}");
    let v = json(&text);
    assert!(v.get("key").is_none(), "no key in the response: {text}");
    assert_eq!(field(&v, "legacyAuth").as_bool(), Some(false));
    let id = text_of(&v, "id");
    let path = format!("/v1/c/{id}");
    assert_eq!(call(&app, &token, "GET", &path, None).0, 200);
    assert_eq!(call(&app, key, "GET", &path, None).0, 401);
}

#[test]
fn a_bad_auth_hash_is_refused() {
    let app = app();
    for bad in ["short", &"G".repeat(64), &"A".repeat(64)] {
        let body = format!(r#"{{"auth_hash":"{bad}"}}"#);
        let (status, _, _) = send(
            &app,
            Request::new("POST", "/calendars")
                .with_header("content-type", "application/json")
                .with_header("accept", "application/json")
                .with_body(body.into_bytes()),
        );
        assert_eq!(status, 400, "{bad}");
    }
}

#[test]
fn sealed_files_round_trip_and_bind_to_the_calendar() {
    let data = br#"{"events":[1,2,3]}"#;
    let sealed = seal_file("k", "cal_a", data).unwrap();
    assert!(sealed.starts_with(b"ALMF1\n"));
    assert!(!sealed.windows(8).any(|w| w == b"\"events\""));
    assert_eq!(open_file("k", "cal_a", &sealed).unwrap(), data);
    assert!(open_file("k", "cal_b", &sealed).is_err());
    assert!(open_file("other", "cal_a", &sealed).is_err());
    let mut bad = sealed.clone();
    let last = bad.len() - 1;
    bad[last] ^= 1;
    assert!(open_file("k", "cal_a", &bad).is_err());
}

fn text_of(v: &agentcloud::json::Value, key: &str) -> String {
    text(v, key).to_string()
}
