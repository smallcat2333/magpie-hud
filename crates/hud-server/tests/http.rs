use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use magpie_hud_server::{
    auth::{random_secret, KeyDatabase},
    router, App, ServerConfig,
};
use serde_json::Value;
use std::time::Duration;
use tower::ServiceExt;

#[tokio::test]
async fn authentication_origin_and_sse_revocation_work_end_to_end() {
    let directory = std::env::temp_dir().join(format!("hud-http-{}", random_secret()));
    std::fs::create_dir_all(&directory).unwrap();
    let log = directory.join("usage.jsonl");
    let private_error = "private upstream diagnostic payload";
    let row = serde_json::json!({"t":chrono::Local::now().to_rfc3339(),"session":"failed-session","providerAccount":"test@example.test","agent":"claude","model":"group/test","status":400,"err":private_error});
    std::fs::write(&log, format!("{row}\n")).unwrap();
    // Use a closed ephemeral port, never the user's local Magpie gateway.
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let magpie_host = reservation.local_addr().unwrap().to_string();
    drop(reservation);
    let mut db = KeyDatabase::default();
    let (key_id, key) = db.create("integration".into());
    db.save(&directory).unwrap();
    let app = App::start(ServerConfig {
        data_dir: directory.clone(),
        usage_jsonl_path: log,
        magpie_host,
        ..ServerConfig::default()
    })
    .unwrap();
    let api = router(app.clone());
    let response = api
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/snapshot")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = api
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/login")
                .header("host", "127.0.0.1:3435")
                .header("origin", "https://attacker.invalid")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"key":key}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = api
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/login")
                .header("host", "127.0.0.1:3435")
                .header("origin", "http://127.0.0.1:3435")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"key":key}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let set_cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(set_cookie.contains("HttpOnly") && set_cookie.contains("SameSite=Strict"));
    assert!(!set_cookie.contains(&key));
    let cookie = set_cookie.split(';').next().unwrap();
    let response = api
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/snapshot")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let snapshot: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(snapshot["instanceId"].as_str().is_some());
    assert!(snapshot["snapshot"].is_object());
    assert!(!String::from_utf8_lossy(&bytes).contains(directory.to_string_lossy().as_ref()));
    assert!(
        !String::from_utf8_lossy(&bytes).contains(private_error),
        "provider errors must not leak through repeated note text"
    );
    let response = api
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/query/not_a_command")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = api
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/events")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let first = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap();
    assert!(String::from_utf8_lossy(&first).contains("event: snapshot"));
    db.keys.remove(&key_id);
    db.save(&directory).unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(3), body.frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap();
    assert!(String::from_utf8_lossy(&frame).contains("auth-expired"));
    let response = api
        .oneshot(
            Request::builder()
                .uri("/api/v1/snapshot")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let service = app.service.clone();
    tokio::task::spawn_blocking(move || service.stop())
        .await
        .unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}
