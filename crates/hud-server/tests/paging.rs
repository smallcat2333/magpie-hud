use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use magpie_hud_server::{auth::{random_secret, KeyDatabase}, router, App, ServerConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

#[tokio::test]
async fn large_details_cross_http_as_overview_and_bounded_pages() {
    let directory = std::env::temp_dir().join(format!("hud-page-http-{}", random_secret()));
    std::fs::create_dir_all(&directory).unwrap();
    let log = directory.join("usage.jsonl");
    let now = chrono::Local::now();
    let date = now.date_naive().to_string();
    let model = "large-model-".to_owned() + &"x".repeat(900);
    {
        use std::io::Write;
        let mut file = std::io::BufWriter::new(std::fs::File::create(&log).unwrap());
        for index in 0..20_000 {
            writeln!(file, "{}", json!({"route_id": index, "t":now.to_rfc3339(), "session":"test-session", "agent":"claude", "model":model, "provider":"fixture", "providerAccount":"fixture", "status":200, "in":1,"out":2,"cache_read":0})).unwrap();
        }
    }
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let magpie_host = reservation.local_addr().unwrap().to_string();
    drop(reservation);
    let mut db = KeyDatabase::default();
    let (_, key) = db.create("paging-test".into());
    db.save(&directory).unwrap();
    let app = App::start(ServerConfig { usage_jsonl_path:log, data_dir:directory.clone(), magpie_host, ..ServerConfig::default() }).unwrap();
    let api = router(app.clone());
    let response = api.clone().oneshot(Request::builder()
        .method("POST").uri("/api/v1/query/query_usage_details_overview")
        .header("authorization",format!("Bearer {key}")).header("content-type","application/json")
        .body(Body::from(json!({"start":date,"end":date}).to_string())).unwrap()).await.unwrap();
    assert_eq!(response.status(), 200);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert!(bytes.len() < 64*1024, "overview must not serialize full request rows");
    let overview: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(overview["total"],20_000);
    assert!(overview.get("rows").is_none());
    let dataset_id = overview["datasetId"].as_u64().unwrap();
    let response = api.clone().oneshot(Request::builder()
        .method("POST").uri("/api/v1/query/query_usage_details_view")
        .header("authorization",format!("Bearer {key}")).header("content-type","application/json")
        .body(Body::from(json!({"query":{"datasetId":dataset_id,"mode":"requests","page":1}}).to_string())).unwrap()).await.unwrap();
    assert_eq!(response.status(), 200);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert!(bytes.len() < 128*1024,"page response must be bounded well below the desktop transport limit");
    let page: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(page["rows"].as_array().unwrap().len(),30);
    assert_eq!(page["totalEntries"],20_000);
    assert_eq!(page["page"],1);
    let response = api.oneshot(Request::builder()
        .uri("/api/v1/capabilities").header("authorization",format!("Bearer {key}"))
        .body(Body::empty()).unwrap()).await.unwrap();
    let caps: Value=serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let zone=caps["timezone"].as_str().unwrap();
    assert!(!zone.contains(' '), "capabilities must carry an Intl-compatible timezone identifier");
    assert!(caps["serverNow"].as_str().is_some());
    let service=app.service.clone();
    tokio::task::spawn_blocking(move||service.stop()).await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}
