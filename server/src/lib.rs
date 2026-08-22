//! mdm-server：HTTP 层。目录镜像路由（routes）+ JS actor 线程桥（actor）+ axum 装配（本文件）。

pub mod actor;
pub mod routes;
pub mod ws;

use std::collections::HashMap;
use std::path::PathBuf;

use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::Response;
use axum::routing::any;
use axum::Router;
use serde_json::Value;

use crate::actor::JsActor;
use crate::routes::Routes;
use mdm_base_rust::bridge::{fail, RequestInfo};

/// 共享状态（JsActor 句柄 Clone = 同一 actor 队列的多份引用）。
#[derive(Clone)]
pub struct AppState {
    routes: Routes,
    actor: JsActor,
    /// 单请求超时（None = 不限时）。
    timeout: Option<std::time::Duration>,
}

/// 构造 axum 应用：catch-all fallback（对齐 Go fiber 的 `All("/*")`）。
pub fn app(
    base: &str,
    dir: impl Into<PathBuf>,
    ts: bool,
    actor: JsActor,
    timeout: Option<std::time::Duration>,
) -> Router {
    Router::new()
        .fallback(any(handle))
        .with_state(AppState {
            routes: Routes::new(base, dir, ts),
            actor,
            timeout,
        })
}

/// 绑定监听并服务。
pub async fn serve(
    addr: std::net::SocketAddr,
    base: &str,
    dir: impl Into<PathBuf>,
    ts: bool,
    actor: JsActor,
    timeout: Option<std::time::Duration>,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve_with_listener(listener, base, dir, ts, actor, timeout).await
}

/// 已绑定监听上服务（测试/T11：先 bind 端口 0 再读 local_addr）。
pub async fn serve_with_listener(
    listener: tokio::net::TcpListener,
    base: &str,
    dir: impl Into<PathBuf>,
    ts: bool,
    actor: JsActor,
    timeout: Option<std::time::Duration>,
) -> std::io::Result<()> {
    axum::serve(listener, app(base, dir, ts, actor, timeout)).await
}

async fn handle(
    State(st): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let Some(file) = st.routes.resolve(uri.path()) else {
        return fail_response(404, "no api file for route");
    };
    let Some(m) = crate::routes::method_name(method.as_str()) else {
        return fail_response(405, &format!("method {method} not mapped"));
    };
    let req = RequestInfo {
        method: method.as_str().to_string(),
        params: HashMap::new(), // 目录镜像路由无路径参数
        query: parse_query(uri.query()),
        headers: headers
            .iter()
            .filter_map(|(k, v)| Some((k.to_string(), v.to_str().ok()?.to_string())))
            .collect(),
        body: body.to_vec(),
    };
    match st.actor.run_module(file, m.to_string(), req, st.timeout).await {
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
pub(crate) mod tests {
    use super::*;
    use mdm_base_rust::bridge::{Bridge, InMemoryKV, LoaderShared, SchemaRegistry};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub(crate) struct TempRoutes(pub(crate) PathBuf);
    pub(crate) fn routes(files: &[(&str, &str)]) -> TempRoutes {
        static N: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "oj-server-{}-{}",
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

    /// 带 oj 模块加载器的 actor（project_root = 路由根：api 文件全在其下，clamp 可达）。
    pub(crate) fn actor(root: PathBuf, ts: bool) -> JsActor {
        JsActor::new(move || {
            Bridge::with_dbs_and_loader(
                HashMap::new(),
                Arc::new(InMemoryKV::new()),
                SchemaRegistry::new(),
                false,
                Some(Arc::new(LoaderShared { project_root: root.clone(), ts })),
            )
        })
    }

    pub(crate) async fn spawn_server(
        base: &str,
        dir: PathBuf,
        ts: bool,
        timeout: Option<std::time::Duration>,
    ) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = base.to_string();
        tokio::spawn(async move {
            serve_with_listener(listener, &base, dir.clone(), ts, actor(dir, ts), timeout)
                .await
                .unwrap();
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
    async fn serves_mirror_route_with_envelope() {
        let t = routes(&[(
            "user/account/api.ts",
            r#"export default { get() { json.ok({ m: http.method, q: http.param("id", 0) }); } };"#,
        )]);
        let addr = spawn_server("/v1/api", t.0.clone(), true, None).await;
        let resp = raw_http(
            addr,
            "GET /v1/api/user/account/?id=7 HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        assert!(resp.contains("\"q\":\"7\""), "{resp}");
        assert!(resp.contains("\"m\":\"GET\""), "{resp}");
    }

    #[tokio::test]
    async fn missing_api_is_404_and_unmapped_verb_405() {
        let t = routes(&[]);
        let addr = spawn_server("/v1/api", t.0.clone(), true, None).await;
        let resp = raw_http(
            addr,
            "GET /v1/api/none/here/ HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 404"), "{resp}");
        // api 文件在但未导出 del → 405（driver 侧 json.fail(405) 信封）。
        let t2 = routes(&[("u/f/api.ts", "export default { get() { json.ok({}); } };")]);
        let addr2 = spawn_server("/v1/api", t2.0.clone(), true, None).await;
        let resp2 = raw_http(
            addr2,
            "DELETE /v1/api/u/f/ HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(resp2.starts_with("HTTP/1.1 405"), "{resp2}");
    }

    #[tokio::test]
    async fn handler_timeout_returns_408_envelope() {
        let t = routes(&[("u/f/api.ts", "export default { get() { while (true) {} } };")]);
        let addr =
            spawn_server("/v1/api", t.0.clone(), true, Some(std::time::Duration::from_millis(200))).await;
        let resp = raw_http(
            addr,
            "GET /v1/api/u/f/ HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 408"), "{resp}");
    }

    #[tokio::test]
    async fn handler_error_returns_500_envelope() {
        let t = routes(&[("u/f/api.ts", "function {{{{\nexport default {};")]);
        let addr = spawn_server("/v1/api", t.0.clone(), true, None).await;
        let resp = raw_http(
            addr,
            "GET /v1/api/u/f/ HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 500"), "{resp}");
    }

    // 静态断言：axum state 可跨线程（Send 边界）。
    fn _assert_send() {
        fn takes_send<T: Send>() {}
        takes_send::<JsActor>();
        takes_send::<AppState>();
    }
}
