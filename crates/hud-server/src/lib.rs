pub mod auth;

use auth::{Auth, SESSION_SECONDS};
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use futures_core::Stream;
use hud_core::service::{Service, ServiceConfig};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    convert::Infallible,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{watch, Semaphore};

#[derive(RustEmbed)]
#[folder = "../../ui/"]
struct Assets;

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerConfig {
    pub listen: SocketAddr,
    pub usage_jsonl_path: PathBuf,
    pub magpie_host: String,
    pub data_dir: PathBuf,
    /// External origin, e.g. https://hud.example.com, without any path.
    pub public_url: Option<String>,
    /// Set before starting worker threads. The server owns day boundaries.
    pub timezone: Option<String>,
    /// Friendly device names for independent Magpie caller keys. Labels do not
    /// change the caller key identity or provide hardware authentication.
    pub caller_key_aliases: std::collections::BTreeMap<String, String>,
}
impl Default for ServerConfig {
    fn default() -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            listen: "127.0.0.1:3435".parse().unwrap(),
            usage_jsonl_path: home.join(".config/magpie/usage.jsonl"),
            magpie_host: "127.0.0.1:3425".into(),
            data_dir: home.join(".config/magpie-hud-server"),
            public_url: None,
            timezone: None,
            caller_key_aliases: Default::default(),
        }
    }
}
impl ServerConfig {
    pub fn validate(&self) -> Result<(), String> {
        hud_core::live::gateway_url(&self.magpie_host)?;
        if let Some(origin) = &self.public_url {
            let url = url::Url::parse(origin).map_err(|_| "publicUrl 格式无效")?;
            if url.scheme() != "https"
                || url.host_str().is_none()
                || url.path() != "/"
                || url.query().is_some()
                || url.fragment().is_some()
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err("publicUrl 必须是无路径、凭据或查询参数的 HTTPS 站点地址".into());
            }
        }
        if !self.listen.ip().is_loopback() && self.public_url.is_none() {
            return Err("对外监听须配置 publicUrl 并通过 HTTPS 反向代理访问".into());
        }
        if !self.usage_jsonl_path.is_absolute() || !self.data_dir.is_absolute() {
            return Err("日志和数据目录必须使用绝对路径".into());
        }
        Ok(())
    }
}

pub struct App {
    pub service: Arc<Service>,
    pub config: ServerConfig,
    pub auth: Auth,
    pub instance_id: String,
    pub boot_id: String,
    current: watch::Sender<Arc<Value>>,
    queries: Arc<Semaphore>,
    shutdown: AtomicBool,
}
impl App {
    pub fn start(config: ServerConfig) -> Result<Arc<Self>, String> {
        config.validate()?;
        auth::private_directory(&config.data_dir)?;
        let instance_path = config.data_dir.join("instance-id");
        let instance_id = match std::fs::read_to_string(&instance_path) {
            Ok(id) if id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()) => id,
            Ok(_) => return Err("HUD 实例标识无效，请恢复原数据目录".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                use std::io::Write;
                let id = auth::random_secret();
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(instance_path)
                    .map_err(|_| "无法创建 HUD 实例标识")?;
                file.write_all(id.as_bytes())
                    .map_err(|_| "无法写入 HUD 实例标识")?;
                id
            }
            Err(_) => return Err("无法读取 HUD 实例标识".into()),
        };
        let service = Service::start(ServiceConfig {
            usage_path: config.usage_jsonl_path.clone(),
            magpie_host: config.magpie_host.clone(),
            cache_dir: config.data_dir.join("cache"),
            include_unassociated: true,
        });
        let (current, _) = watch::channel(Arc::new(Value::Null));
        let app = Arc::new(Self {
            auth: Auth::new(config.data_dir.clone()),
            config,
            service,
            instance_id,
            boot_id: auth::random_secret(),
            current,
            queries: Arc::new(Semaphore::new(8)),
            shutdown: AtomicBool::new(false),
        });
        app.publish();
        Ok(app)
    }
    /// Stop accepting work and wake open SSE streams before collectors stop.
    pub fn shutdown(&self) {
        if self.shutdown.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.current.send(Arc::new(json!({"shutdown":true})));
    }
    /// Full snapshots are coalesced in one watch slot. Slow readers never block
    /// collectors and always receive a fresh complete state on subscription.
    pub fn publish(&self) {
        if self.shutdown.load(Ordering::Acquire) {
            return;
        }
        let previous_revision = self.current.borrow().get("revision").and_then(Value::as_u64);
        let Some(snapshot) = self.service.snapshot_if_changed(previous_revision) else { return };
        let revision = snapshot.revision;
        let mut value = serde_json::to_value(snapshot.as_ref()).unwrap_or(Value::Null);
        self.scrub(&mut value);
        self.current.send_replace(Arc::new(json!({
            "instanceId": self.instance_id, "bootId": self.boot_id, "revision": revision,
            "collectedAt": chrono::Utc::now().to_rfc3339(),
            "serverNow": chrono::Local::now().to_rfc3339(),
            "timezone": self.config.timezone.clone().unwrap_or_else(hud_core::paths::local_timezone), "snapshot": value,
        })));
    }
    fn scrub(&self, value: &mut Value) {
        // A local note may repeat a provider error. Remove the same raw text
        // there as well as in structured error fields before publishing it.
        fn collect(value: &Value, errors: &mut Vec<String>) {
            match value {
                Value::Object(map) => {
                    for (key, value) in map {
                        if key.to_ascii_lowercase().ends_with("error") {
                            if let Some(text) = value.as_str().filter(|s| !s.is_empty()) {
                                if !errors.iter().any(|old| old == text) {
                                    errors.push(text.into());
                                }
                            }
                        }
                        collect(value, errors);
                    }
                }
                Value::Array(values) => {
                    for value in values {
                        collect(value, errors);
                    }
                }
                _ => {}
            }
        }
        let mut errors = Vec::new();
        collect(value, &mut errors);
        self.scrub_value(value, &errors);
    }
    fn scrub_value(&self, value: &mut Value, errors: &[String]) {
        match value {
            Value::Object(map) => {
                if let Some(name) = map
                    .get("callerKeyId")
                    .and_then(Value::as_str)
                    .and_then(|id| self.config.caller_key_aliases.get(id))
                    .cloned()
                {
                    map.insert("callerKeyName".into(), Value::String(name));
                }
                if let Some(Value::Object(names)) = map.get_mut("callerKeyNames") {
                    for (id, alias) in &self.config.caller_key_aliases {
                        if names.contains_key(id) {
                            names.insert(id.clone(), Value::String(alias.clone()));
                        }
                    }
                }
                // All payload types originate in the HUD DTOs, never Magpie's raw
                // HTTP responses. Strip absolute server paths from text and errors.
                let paths: Vec<String> = [&self.config.usage_jsonl_path, &self.config.data_dir]
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .filter(|p| !p.is_empty())
                    .collect();
                for (key, value) in map.iter_mut() {
                    if let Value::String(text) = value {
                        if key.to_ascii_lowercase().ends_with("error") {
                            *text = "采集或查询暂时不可用，请检查 Magpie 状态".into();
                        } else {
                            if key == "text" || key == "detail" {
                                for error in errors {
                                    *text = text.replace(error, "请求失败（上游详情已隐藏）");
                                }
                            }
                            for path in &paths {
                                if text.contains(path.as_str()) {
                                    *text = text.replace(path.as_str(), "[服务器数据]");
                                }
                            }
                        }
                    } else {
                        self.scrub_value(value, errors);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.scrub_value(item, errors);
                }
            }
            Value::String(text) => {
                for path in [&self.config.usage_jsonl_path, &self.config.data_dir] {
                    let path = path.to_string_lossy();
                    if !path.is_empty() && text.contains(path.as_ref()) {
                        *text = text.replace(path.as_ref(), "[服务器数据]");
                    }
                }
            }
            _ => {}
        }
    }
    fn origin_matches(&self, headers: &HeaderMap) -> bool {
        let Some(origin) = headers.get(header::ORIGIN).and_then(|h| h.to_str().ok()) else {
            return false;
        };
        let expected = self.config.public_url.clone().unwrap_or_else(|| {
            format!(
                "http://{}",
                headers
                    .get(header::HOST)
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or("")
            )
        });
        let Ok(a) = url::Url::parse(origin) else {
            return false;
        };
        let Ok(b) = url::Url::parse(&expected) else {
            return false;
        };
        // In direct HTTP mode only loopback browser origins are accepted.
        if self.config.public_url.is_none()
            && !matches!(
                a.host_str(),
                Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
            )
        {
            return false;
        }
        a.origin() == b.origin()
    }
}

pub fn router(app: Arc<App>) -> Router {
    let api = Router::new()
        .route("/capabilities", get(capabilities))
        .route("/snapshot", get(snapshot))
        .route("/events", get(events))
        .route("/query/{command}", post(query))
        .route("/logout", post(logout))
        .route_layer(middleware::from_fn_with_state(app.clone(), authorize));
    Router::new()
        .route(
            "/api/v1/health",
            get(|| async { Json(json!({"status":"ok","protocolVersion":1})) }),
        )
        .route("/api/v1/login", post(login))
        .nest("/api/v1", api)
        .route("/", get(index))
        .route("/{*path}", get(asset))
        .layer(DefaultBodyLimit::max(65_536))
        .layer(middleware::from_fn(response_headers))
        .with_state(app)
}
async fn response_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    headers.insert("referrer-policy", "no-referrer".parse().unwrap());
    headers.insert("content-security-policy", "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; form-action 'self'; base-uri 'self'".parse().unwrap());
    response
}
fn failure(status: StatusCode, text: &str) -> Response {
    (status, Json(json!({"error":text}))).into_response()
}
fn cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|v| v.trim().strip_prefix("hud_session="))
}
fn identity(app: &App, headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers.get(header::AUTHORIZATION) {
        return app
            .auth
            .verify_key(value.to_str().ok()?.strip_prefix("Bearer ")?);
    }
    app.auth.session_key(cookie(headers)?)
}
async fn authorize(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    if identity(&app, request.headers()).is_none() {
        return failure(StatusCode::UNAUTHORIZED, "请登录或检查 HUD 查看密钥");
    }
    if request.method() != axum::http::Method::GET
        && !request.headers().contains_key(header::AUTHORIZATION)
        && !app.origin_matches(request.headers())
    {
        return failure(StatusCode::FORBIDDEN, "请求来源无效");
    }
    next.run(request).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    key: String,
}
async fn login(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(body): Json<Login>,
) -> Response {
    if !app.origin_matches(&headers) {
        return failure(StatusCode::FORBIDDEN, "请求来源无效");
    }
    if !app.auth.allow_login() {
        return failure(StatusCode::TOO_MANY_REQUESTS, "登录尝试过多，请稍后再试");
    }
    let Some(token) = app.auth.login(&body.key) else {
        return failure(StatusCode::UNAUTHORIZED, "HUD 查看密钥无效");
    };
    let secure = if app.config.public_url.is_some() {
        "; Secure"
    } else {
        ""
    };
    let value = format!("hud_session={token}; Path=/api/v1; HttpOnly; SameSite=Strict; Max-Age={SESSION_SECONDS}{secure}");
    ([(header::SET_COOKIE, value)], Json(json!({"ok":true}))).into_response()
}
async fn logout(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if let Some(token) = cookie(&headers) {
        app.auth.logout(token);
    }
    (
        [(
            header::SET_COOKIE,
            "hud_session=; Path=/api/v1; HttpOnly; SameSite=Strict; Max-Age=0",
        )],
        Json(json!({"ok":true})),
    )
        .into_response()
}
async fn capabilities(State(app): State<Arc<App>>) -> Json<Value> {
    let mut health = app.service.health();
    app.scrub(&mut health);
    Json(
        json!({"protocolVersion":1,"instanceId":app.instance_id,"bootId":app.boot_id,
        "timezone":app.config.timezone.clone().unwrap_or_else(hud_core::paths::local_timezone),
        "serverNow":chrono::Local::now().to_rfc3339(),
        "features":{"fullSnapshots":true,"history":true,"details":true,"detailsPaged":true,"sourceFilter":true},"collectors":health}),
    )
}
async fn snapshot(State(app): State<Arc<App>>) -> Json<Value> {
    Json((**app.current.borrow()).clone())
}
async fn query(
    State(app): State<Arc<App>>,
    Path(command): Path<String>,
    Json(args): Json<Value>,
) -> Response {
    if !matches!(
        command.as_str(),
        "query_date_stats"
            | "query_month_usage"
            | "query_route_history"
            | "query_usage_details"
            | "query_usage_details_overview"
            | "query_usage_details_view"
            | "query_hourly_costs"
            | "query_model_costs"
    ) {
        return failure(StatusCode::NOT_FOUND, "未知查询");
    }
    let Ok(permit) = app.queries.clone().try_acquire_owned() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "查询繁忙，请稍后重试");
    };
    let service = app.service.clone();
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        service.query(&command, args)
    })
    .await
    {
        Ok(Ok(mut value)) => {
            app.scrub(&mut value);
            Json(value).into_response()
        }
        Ok(Err(_)) => failure(
            StatusCode::BAD_REQUEST,
            "查询失败，请检查日期、范围或服务器数据状态",
        ),
        Err(_) => failure(StatusCode::INTERNAL_SERVER_ERROR, "查询暂时不可用"),
    }
}
async fn events(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut receiver = app.current.subscribe();
    let stream = async_stream::stream! {
        let mut check = tokio::time::interval(Duration::from_secs(1));
        // subscribe + borrow_and_update occurs before waiting for changes: no
        // GET/subscription gap, even when collectors publish during handshakes.
        let initial = receiver.borrow_and_update().clone();
        yield Ok(Event::default().event("snapshot").id(event_id(&initial)).data(initial.to_string()));
        loop {
            tokio::select! {
                changed = receiver.changed() => {
                    if changed.is_err() { break; }
                    if identity(&app, &headers).is_none() { yield Ok(Event::default().event("auth-expired").data("{}")); break; }
                    let value = receiver.borrow_and_update().clone();
                    if value.get("shutdown").and_then(Value::as_bool) == Some(true) { break; }
                    yield Ok(Event::default().event("snapshot").id(event_id(&value)).data(value.to_string()));
                }
                _ = check.tick() => {
                    if identity(&app, &headers).is_none() { yield Ok(Event::default().event("auth-expired").data("{}")); break; }
                }
            }
        }
    };
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("hud-heartbeat"),
    )
}
fn event_id(value: &Value) -> String {
    format!(
        "{}:{}",
        value["bootId"].as_str().unwrap_or(""),
        value["revision"].as_u64().unwrap_or(0)
    )
}
async fn index() -> Response {
    static_asset("index.html")
}
async fn asset(Path(path): Path<String>) -> Response {
    static_asset(&path)
}
fn static_asset(path: &str) -> Response {
    let Some(asset) = Assets::get(path) else {
        return failure(StatusCode::NOT_FOUND, "资源不存在");
    };
    let mime = mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string();
    (
        [(header::CONTENT_TYPE, mime)],
        Body::from(asset.data.into_owned()),
    )
        .into_response()
}

pub async fn serve(config: ServerConfig) -> Result<(), String> {
    config.validate()?;
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|_| "无法监听 HUD 端口")?;
    let app = App::start(config)?;
    eprintln!("Magpie HUD Server listening on {}", app.config.listen);
    let publisher = app.clone();
    let task = tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_millis(250));
        loop {
            timer.tick().await;
            publisher.publish();
        }
    });
    let stopping = app.clone();
    let result = axum::serve(listener, router(app.clone()))
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            stopping.shutdown();
        })
        .await;
    task.abort();
    app.shutdown();
    app.service.stop();
    result.map_err(|_| "HUD HTTP 服务退出异常".into())
}
/// systemd stops a unit with SIGTERM; handle it like Ctrl-C so collectors and
/// keychain-independent state finish before process exit.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("无法注册终止信号");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
