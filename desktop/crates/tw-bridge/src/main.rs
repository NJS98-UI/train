//! tw-bridge: 浏览器预览与集成测试用的 IPC 桥（与 Tauri 壳共用同一 tw-core）。
//! 8787 是网关端口（经 invoke service_start 拉起）；本进程默认 8790 提供
//! POST /api/invoke/{cmd} 与 GET /api/events(SSE)。

use axum::body::Body;
use axum::extract::{Path as AxPath, Request, State};
use axum::http::{HeaderName, HeaderValue, Response, StatusCode};
use axum::middleware::{from_fn, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

#[derive(Clone)]
struct Hub(pub tokio::sync::broadcast::Sender<(String, serde_json::Value)>);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut data_dir = std::env::var("TW_DATA_DIR").unwrap_or_else(|_| {
        dirs::home_dir().unwrap_or_default().join(".traework2api-desktop-preview").to_string_lossy().to_string()
    });
    let mut ipc_port = 8790u16;
    let mut auto_gateway = true;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--data-dir" => data_dir = args.next().unwrap_or_default(),
            "--ipc-port" => ipc_port = args.next().unwrap_or_default().parse().unwrap_or(8790),
            "--no-gateway" => auto_gateway = false,
            _ => {}
        }
    }
    tw_core::appcore::init(&data_dir);
    eprintln!("[tw-bridge] 数据目录: {}", data_dir);

    let (tx, _rx) = tokio::sync::broadcast::channel::<(String, serde_json::Value)>(256);
    let hub = Hub(tx.clone());
    tw_core::emitter::set_emitter(Arc::new(move |topic: &str, payload: serde_json::Value| {
        let _ = tx.send((topic.to_string(), payload));
    }));

    if auto_gateway && tw_core::appcore::intent::should_autostart_on_open() {
        if let Err(e) = tw_core::appcore::gateway_start().await {
            eprintln!("[tw-bridge] 自动启动网关失败: {}", e);
        }
    }

    let addr = format!("127.0.0.1:{}", ipc_port);
    eprintln!("[tw-bridge] IPC 监听 http://{}", addr);
    let app = Router::new()
        .route("/api/invoke/{cmd}", post(invoke))
        .route("/api/events", get(events))
        .route("/api/healthz", get(|| async { "ok" }))
        .layer(from_fn(cors))
        .with_state(hub);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn cors(req: Request<Body>, next: Next) -> Response<Body> {
    let is_options = req.method() == axum::http::Method::OPTIONS;
    let resp = if is_options {
        Response::builder().status(StatusCode::NO_CONTENT).body(Body::empty()).unwrap()
    } else {
        next.run(req).await
    };
    let mut resp = resp;
    for (k, v) in [
        ("access-control-allow-origin", "*"),
        ("access-control-allow-headers", "content-type,authorization"),
        ("access-control-allow-methods", "GET,POST,OPTIONS"),
        ("access-control-max-age", "86400"),
    ] {
        if let (Ok(n), Ok(val)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
            resp.headers_mut().insert(n, val);
        }
    }
    resp
}

async fn invoke(State(_hub): State<Hub>, AxPath(cmd): AxPath<String>, Json(args): Json<serde_json::Value>) -> Json<serde_json::Value> {
    match tw_core::ipc::call(&cmd, args).await {
        Ok(data) => Json(serde_json::json!({ "ok": true, "data": data })),
        Err(error) => Json(serde_json::json!({ "ok": false, "error": error })),
    }
}

async fn events(State(hub): State<Hub>) -> Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let rx = hub.0.subscribe();
    Sse::new(BroadcastStream::new(rx).filter_map(|item| match item {
        Ok((topic, payload)) => Some(Ok(Event::default().event(topic).data(payload.to_string()))),
        Err(_) => None,
    }))
}

#[allow(dead_code)]
fn _type_anchor(_: KeepAlive) {}
