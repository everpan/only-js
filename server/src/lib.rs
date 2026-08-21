//! mdm-server：HTTP 层。路由解析（router）+ JS actor 线程桥（actor）+ axum 装配（本文件）。

pub mod actor;
pub mod devserver;
pub mod router;

use std::collections::HashMap;
use std::path::PathBuf;

use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::Response;
use axum::routing::any;
use axum::Router;
use serde_json::Value;

use crate::actor::JsActor;
use crate::router::{FileResolver, Resolver};
use mdm_base_rust::bridge::{fail, RequestInfo};

/// 共享状态（JsActor 句柄 Clone = 同一 actor 队列的多份引用）。
#[derive(Clone)]
pub struct AppState {
    resolver: FileResolver,
    actor: JsActor,
    /// 单请求超时（None = 不限时）。
    timeout: Option<std::time::Duration>,
}

/// 构造 axum 应用：catch-all fallback（对齐 Go fiber 的 `All("/*")`）。
pub fn app(
    base_dir: impl Into<PathBuf>,
    actor: JsActor,
    timeout: Option<std::time::Duration>,
) -> Router {
    Router::new()
        .fallback(any(handle))
        .with_state(AppState {
            resolver: FileResolver::new(base_dir),
            actor,
            timeout,
        })
}

/// 绑定监听并服务。
pub async fn serve(
    addr: std::net::SocketAddr,
    base_dir: impl Into<PathBuf>,
    actor: JsActor,
    timeout: Option<std::time::Duration>,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app(base_dir, actor, timeout)).await
}

async fn handle(
    State(st): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let (file, p) = match st.resolver.resolve(method.as_str(), uri.path()) {
        Some(x) => x,
        None => return fail_response(404, "route not resolved"),
    };
    // Go dev 语义：resolve 出文件即读即执行（per-request 读盘 = 免费热重载）。
    let source = match std::fs::read_to_string(&file) {
        Ok(s) => s,
        Err(e) => return fail_response(500, &format!("read handler: {e}")),
    };
    let req = RequestInfo {
        method: method.as_str().to_string(),
        params: HashMap::from([
            ("sub".to_string(), p.sub),
            ("feature".to_string(), p.feature),
            ("entity".to_string(), p.entity),
        ]),
        query: parse_query(uri.query()),
        headers: headers
            .iter()
            .filter_map(|(k, v)| Some((k.to_string(), v.to_str().ok()?.to_string())))
            .collect(),
        body: body.to_vec(),
    };
    match st.actor.run(source, req, st.timeout).await {
        Ok(cap) => capture_response(cap),
        // 超时熔断 → 408（对齐 Go dev server）。
        Err(e) if e.timeout => fail_response(408, &e.msg),
        Err(e) => fail_response(500, &e.msg),
    }
}

/// Capture → axum Response（status/headers/body 原样回写）。
fn capture_response(cap: mdm_base_rust::bridge::Capture) -> Response {
    let mut r = Response::new(axum::body::Body::from(cap.body));
    *r.status_mut() = StatusCode::from_u16(cap.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    for (k, v) in cap.headers {
        if let (Ok(name), Ok(hv)) = (
            k.parse::<axum::http::HeaderName>(),
            v.parse::<axum::http::HeaderValue>(),
        ) {
            r.headers_mut().insert(name, hv);
        }
    }
    r
}

/// 统一失败信封。
fn fail_response(code: i32, msg: &str) -> Response {
    let (body, status) = fail(code, msg, &Value::Null);
    let mut r = Response::new(axum::body::Body::from(body));
    *r.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    r
}

/// `a=1&b=2` → map（ponytail: 未做 percent-decode，出现编码值时再补）。
fn parse_query(q: Option<&str>) -> HashMap<String, String> {
    q.map(|s| {
        s.split('&')
            .filter(|kv| !kv.is_empty())
            .map(|kv| match kv.split_once('=') {
                Some((k, v)) => (k.to_string(), v.to_string()),
                None => (kv.to_string(), String::new()),
            })
            .collect()
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdm_base_rust::bridge::{Bridge, InMemoryAccessor, InMemoryKV};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct TempRoutes(PathBuf);
    fn routes(files: &[(&str, &str)]) -> TempRoutes {
        static N: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "mdm-server-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&base).unwrap();
        for (rel, content) in files {
            let p = base.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        TempRoutes(base)
    }
    impl Drop for TempRoutes {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn actor() -> JsActor {
        JsActor::new(|| {
            Bridge::new(
                Arc::new(InMemoryAccessor::new()),
                Arc::new(InMemoryKV::new()),
            )
        })
    }

    async fn spawn_server(base: PathBuf, timeout: Option<std::time::Duration>) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app(base, actor(), timeout)).await.unwrap();
        });
        addr
    }

    async fn raw_http(addr: std::net::SocketAddr, req: &str) -> String {
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        s.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    }

    #[tokio::test]
    async fn serves_route_with_envelope() {
        let t = routes(&[(
            "crm-v1/user/profile/list/GET.js",
            r#"json.ok({ m: http.method, e: http.params.entity, q: http.query.id });"#,
        )]);
        let addr = spawn_server(t.0.clone(), None).await;
        let resp = raw_http(
            addr,
            "GET /crm-v1/user/profile/list?id=7 HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        assert!(resp.contains("\"code\":0"), "{resp}");
        assert!(resp.contains("\"e\":\"list\""), "{resp}");
        assert!(resp.contains("\"q\":\"7\""), "{resp}");
        assert!(resp.contains("\"m\":\"GET\""), "{resp}");
    }

    #[tokio::test]
    async fn unknown_route_returns_404_envelope() {
        let t = routes(&[]);
        let addr = spawn_server(t.0.clone(), None).await;
        let resp = raw_http(
            addr,
            "GET /crm-v1/nope/missing/thing HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 404"), "{resp}");
        assert!(resp.contains("\"code\":404"), "{resp}");
    }

    #[tokio::test]
    async fn handler_timeout_returns_408_envelope() {
        let t = routes(&[("crm-v1/user/profile/list/GET.js", "while (true) {}")]);
        let addr = spawn_server(t.0.clone(), Some(std::time::Duration::from_millis(150))).await;
        let resp = raw_http(
            addr,
            "GET /crm-v1/user/profile/list HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 408"), "{resp}");
        assert!(resp.contains("\"code\":408"), "{resp}");
        assert!(resp.contains("handler execution timed out"), "{resp}");
    }

    #[tokio::test]
    async fn handler_error_returns_500_envelope() {
        let t = routes(&[("crm-v1/user/profile/list/GET.js", "this is !!! not js")]);
        let addr = spawn_server(t.0.clone(), None).await;
        let resp = raw_http(
            addr,
            "GET /crm-v1/user/profile/list HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 500"), "{resp}");
        assert!(resp.contains("\"code\":500"), "{resp}");
    }

    // 静态断言：axum state 可跨线程（Send 边界）。
    fn _assert_send() {
        fn takes_send<T: Send>() {}
        takes_send::<JsActor>();
        takes_send::<AppState>();
    }
}
