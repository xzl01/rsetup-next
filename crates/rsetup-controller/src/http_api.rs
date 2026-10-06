//! HTTP contract helpers: per-response envelopes and the public user
//! projection, shared by the auth handlers and the front-end mock contract.
//!
//! Contract (spec 02 §3): success is `{data, request_id}`, failure is
//! `{error: {code, message_key, params}, request_id}`; `request_id` is a fresh
//! UUIDv4 per response. The public user projection carries non-sensitive
//! fields only — never the password hash, session digests or tokens.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::auth::service::IdentityUser;

/// Fresh per-response correlation id: a random (v4) UUID, canonical lowercase.
pub fn request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Success envelope: HTTP 200 with exactly `{data, request_id}`.
pub fn ok_response(data: Value) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "data": data,
            "request_id": request_id(),
        })),
    )
        .into_response()
}

/// Failure envelope: the supplied status with exactly
/// `{error: {code, message_key, params}, request_id}`.
pub fn err_response(status: StatusCode, code: &str, message_key: &str, params: Value) -> Response {
    (
        status,
        Json(json!({
            "error": {
                "code": code,
                "message_key": message_key,
                "params": params,
            },
            "request_id": request_id(),
        })),
    )
        .into_response()
}

/// Non-sensitive user projection: canonical lowercase UUID string id, username
/// and strict JSON booleans; `revision` is a decimal string (spec 02 §1).
pub fn user_public(u: &IdentityUser) -> Value {
    json!({
        "id": uuid::Uuid::from_bytes(u.id).to_string(),
        "username": u.username,
        "active": u.active,
        "is_admin": u.is_admin,
        "must_change_password": u.must_change_password,
        "revision": u.revision.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn body_json(resp: Response) -> Value {
        let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn is_uuid_v4(s: &str) -> bool {
        match uuid::Uuid::parse_str(s) {
            Ok(u) => u.get_version_num() == 4,
            Err(_) => false,
        }
    }

    #[tokio::test]
    async fn ok_envelope_shape() {
        let resp = super::ok_response(json!({"changed": true}));
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(
            body["data"],
            json!({"changed": true}),
            "data must equal the input verbatim"
        );
        let keys: Vec<&str> = body
            .as_object()
            .map(|o| o.keys().map(|k| k.as_str()).collect())
            .unwrap_or_default();
        assert_eq!(
            keys.len(),
            2,
            "envelope must contain exactly data + request_id: {body}"
        );
        assert!(
            keys.contains(&"data") && keys.contains(&"request_id"),
            "envelope keys: {body}"
        );
        assert!(
            is_uuid_v4(body["request_id"].as_str().unwrap_or("")),
            "request_id must be a parseable UUID v4: {}",
            body["request_id"]
        );
    }

    #[tokio::test]
    async fn err_envelope_shape() {
        let resp = super::err_response(
            StatusCode::UNAUTHORIZED,
            "INVALID_CREDENTIALS",
            "auth.invalid_credentials",
            json!({}),
        );
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "status must pass through unchanged"
        );
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], json!("INVALID_CREDENTIALS"));
        assert_eq!(
            body["error"]["message_key"],
            json!("auth.invalid_credentials")
        );
        assert_eq!(body["error"]["params"], json!({}));
        let error_keys: Vec<&str> = body["error"]
            .as_object()
            .map(|o| o.keys().map(|k| k.as_str()).collect())
            .unwrap_or_default();
        assert_eq!(
            error_keys.len(),
            3,
            "error must contain exactly code/message_key/params: {body}"
        );
        let top_keys: Vec<&str> = body
            .as_object()
            .map(|o| o.keys().map(|k| k.as_str()).collect())
            .unwrap_or_default();
        assert_eq!(
            top_keys.len(),
            2,
            "envelope must contain exactly error + request_id: {body}"
        );
        assert!(
            top_keys.contains(&"error") && top_keys.contains(&"request_id"),
            "envelope keys: {body}"
        );
        assert!(
            is_uuid_v4(body["request_id"].as_str().unwrap_or("")),
            "request_id must be a parseable UUID v4: {}",
            body["request_id"]
        );
    }

    #[test]
    fn user_public_redacts_and_strict_bool() {
        let id = uuid::Uuid::new_v4();
        let user = IdentityUser {
            id: *id.as_bytes(),
            username: "alice".to_string(),
            password_hash: "$argon2id$=synthetic-test-only=".to_string(),
            active: true,
            is_admin: false,
            must_change_password: true,
            revision: 1,
        };
        let v = super::user_public(&user);
        let obj = v
            .as_object()
            .expect("user_public must project a JSON object");
        assert!(
            !obj.contains_key("password_hash"),
            "projection must not leak password_hash: {v}"
        );
        let rendered = v.to_string();
        assert!(
            !rendered.contains("argon2"),
            "projection must not echo hash material: {rendered}"
        );
        assert_eq!(
            obj.len(),
            6,
            "projection must contain exactly the six public fields: {v}"
        );
        assert_eq!(v["id"], json!(id.to_string()));
        assert_eq!(v["username"], json!("alice"));
        assert_eq!(v["active"], json!(true));
        assert_eq!(v["is_admin"], json!(false));
        assert_eq!(v["must_change_password"], json!(true));
        assert_eq!(v["revision"], json!("1"));
        assert!(
            v["active"].is_boolean()
                && v["is_admin"].is_boolean()
                && v["must_change_password"].is_boolean(),
            "active/is_admin/must_change_password must be strict JSON booleans: {v}"
        );
        let id_s = v["id"].as_str().expect("id must be a string");
        assert_eq!(
            id_s.len(),
            36,
            "id must be the standard hyphenated UUID form, not hex32: {id_s}"
        );
        assert!(
            id_s.contains('-') && id_s == id_s.to_lowercase(),
            "id must be standard lowercase UUID string: {id_s}"
        );
    }
}
