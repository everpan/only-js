//! bridge：注入到 deno_core JsRuntime 的 JS SDK 全局对象（移植自 Go internal/bridge，goja → deno_core）。
//! 按绑定类型分文件组织：
//!   - json.rs     —— json.ok/fail/header 统一信封与返回头
//!   - db.rs       —— db.query/exec 与 DB(name) 命名实例（异步 op，支持绑定参数）
//!   - query.rs    —— 安全查询构造器（sea-query + SchemaRegistry 白名单，参数化值）
//!   - fetch.rs    —— fetch(url, options?) HTTP 客户端（reqwest，异步 Promise）
//!   - http.rs     —— http.* 请求上下文（只读，懒加载）
//!   - kv.rs       —— redis 内存 KV 抽象（get/set，无 Redis 时联调用）
//!   - log.rs      —— log.debug/info/warn/error 结构化日志（tracing）
//!   - envelope.rs —— {code,msg,data} 统一信封
//!   - registry.rs —— SchemaRegistry 表/列白名单（SQL 注入根治点）
//!   - runtime.rs  —— RuntimePool：复用 JsRuntime（预热 = 快照等价），V8 代码缓存
//!   - loader.rs   —— HandlerStore：handler 源码加载 + 热重载（FS / 嵌入）
//!   - inspector.rs—— InspectorServer：deno_core 自带 inspector 的 DevTools WS 桥
//!   - bootstrap.js —— JS 侧全局对象装配
//!
//! 状态拆分（revised）：
//!   - `StableState`：跨请求不变（kv / dbs / client / registry），`Arc` 共享，创建一次。
//!   - `ReqState`：每请求可变（req / 响应捕获 / done），存入 OpState，checkout 时重置。
//! 如此 JsRuntime 可池化复用而不串号请求。

mod db;
mod envelope;
mod fetch;
mod accessor_sqlx;
mod http;
mod inspector;
mod json;
mod kv;
mod loader;
mod log;
mod query;
mod registry;
mod runtime;

pub use db::{DataAccessor, InMemoryAccessor, Row};
pub use accessor_sqlx::SqlxAccessor;
pub use envelope::{fail, ok, status_code};
pub use http::RequestInfo;
pub use kv::{InMemoryKV, KVStore};
pub use loader::HandlerStore;
pub use registry::SchemaRegistry;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use deno_core::{OpState, op2};
use deno_core::error::CoreError;

/// 契约实现（DataAccessor/KVStore）的统一错误返回（stdlib，不泄漏 deno 类型）。
pub type BridgeResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// StableState：跨请求共享、创建后不可变（内部句柄均为 Arc）。
pub struct StableState {
    pub kv: Arc<dyn KVStore>,
    pub dbs: HashMap<String, Arc<dyn DataAccessor>>,
    pub client: reqwest::Client,
    pub registry: Arc<SchemaRegistry>,
}

/// ReqState：每请求可变状态（存在 OpState 中，checkout 时整体重置）。
#[derive(Default, Clone)]
pub struct ReqState {
    pub req: RequestInfo,
    pub response: Option<Vec<u8>>,
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub done: bool,
}

impl ReqState {
    /// 重置为初始态，写入新请求的上下文。
    pub fn reset(&mut self, req: RequestInfo) {
        self.req = req;
        self.response = None;
        self.status = 200;
        self.headers.clear();
        self.done = false;
    }
}

/// 历史兼容别名（部分旧调用可能引用 `Shared`）。
pub type Shared = Rc<RefCell<StableState>>;

deno_core::extension!(
    bridge_ext,
    ops = [
        op_finish,
        json::op_json_ok,
        json::op_json_fail,
        json::op_json_header,
        http::op_http_info,
        kv::op_kv_get,
        kv::op_kv_set,
        db::op_db_has,
        db::op_db_query,
        db::op_db_exec,
        query::op_db_query_build,
        fetch::op_fetch,
        log::op_log,
    ],
    esm_entry_point = "ext:bridge_ext/bootstrap.js",
    esm = [dir "src/bridge", "bootstrap.js"],
    options = { stable: Arc<StableState> },
    state = |state, options| {
        state.put(options.stable.clone());
        state.put(ReqState::default());
    },
);

/// finish()：标记会话完成。
#[op2(fast)]
fn op_finish(state: &mut OpState) {
    state.borrow_mut::<ReqState>().done = true;
}

/// 响应捕获（json.ok/fail 写入，server 层读取后写回 HTTP 响应）。
#[derive(Default, Clone)]
pub struct Capture {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

/// Bridge：持有共享稳定状态、runtime 池、handler 仓库，并执行 handler 脚本。
pub struct Bridge {
    stable: Arc<StableState>,
    pool: runtime::RuntimePool,
    handlers: HandlerStore,
    /// 是否启用 inspector（透传至 runtime 工厂）。
    inspect: bool,
}

impl Bridge {
    /// 构造 Bridge（依赖倒置：传入接口而非具体实现）。
    /// 传入的 db 注册为 dbs["default"]，等价于 DB("default")，无需额外配置。
    pub fn new(db: Arc<dyn DataAccessor>, kv: Arc<dyn KVStore>) -> Self {
        Self::with_opts(db, kv, SchemaRegistry::new(), false)
    }

    /// 含 schema 注册表与 inspector 开关的构造。
    pub fn with_opts(
        db: Arc<dyn DataAccessor>,
        kv: Arc<dyn KVStore>,
        registry: SchemaRegistry,
        inspect: bool,
    ) -> Self {
        let stable = Arc::new(StableState {
            kv,
            dbs: HashMap::from([("default".to_string(), db)]),
            client: reqwest::Client::builder()
                .no_proxy()
                .build()
                .unwrap_or_default(),
            registry: Arc::new(registry),
        });
        let pool = runtime::RuntimePool::new(stable.clone(), inspect);
        Self {
            stable,
            pool,
            handlers: HandlerStore::from_env(),
            inspect,
        }
    }

    /// 合并注入命名 DataAccessor（对应 Go 的 SetDBAccessors），"default" 默认保留。
    /// 注意：须在首次 checkout runtime 之前调用（StableState 一旦被 runtime 共享即不可变）。
    pub fn set_db_accessors<I>(&mut self, m: I)
    where
        I: IntoIterator<Item = (String, Arc<dyn DataAccessor>)>,
    {
        let stable = Arc::get_mut(&mut self.stable)
            .expect("StableState already shared with pooled runtimes; call before warm/run");
        stable.dbs.extend(m);
    }

    /// 替换 handler 仓库（生产用嵌入 map，开发用 FS 热重载）。
    pub fn set_handlers(&mut self, handlers: HandlerStore) {
        self.handlers = handlers;
    }

    /// 取 inspector 句柄（若以 inspect=true 构造且已有 checkout 的 runtime 持有时可用）。
    /// 此处提供便捷构造 inspector server 的能力：需先 checkout 一个 runtime 并取其 inspector。
    pub fn inspect(&self) -> bool {
        self.inspect
    }

    /// 执行 handler 源码并驱动 event loop 至所有 Promise 落定。
    /// 从池借出 runtime，重置 per-request 状态，执行，归还。
    pub async fn run(&self, source: &str) -> Result<Capture, CoreError> {
        self.run_with(source, RequestInfo::default()).await
    }

    /// 带请求上下文执行。
    pub async fn run_with(&self, source: &str, req: RequestInfo) -> Result<Capture, CoreError> {
        let mut rt = self.pool.checkout();
        // 重置 per-request 状态。
        {
            let op_state = runtime::op_state(&rt);
            let mut g = op_state.borrow_mut();
            g.borrow_mut::<ReqState>().reset(req);
        }
        let result = runtime::run_to_completion(&mut rt, "handler.js", source.to_string()).await;
        // 读取捕获。
        let capture = {
            let op_state = runtime::op_state(&rt);
            let g = op_state.borrow();
            let rs = g.borrow::<ReqState>();
            Capture {
                status: rs.status,
                headers: rs.headers.clone(),
                body: rs.response.clone().unwrap_or_default(),
            }
        };
        // 仅成功执行（已轮询 event loop）的 runtime 才归还；失败的可能 isolate 损坏，直接丢弃。
        match result {
            Ok(()) => {
                self.pool.checkin(rt);
                Ok(capture)
            }
            Err(e) => Err(e),
        }
    }

    /// 按名执行已加载的 handler（热重载走 HandlerStore）。
    pub async fn run_named(&self, name: &str) -> Result<Capture, CoreError> {
        let src = self.handlers.get(name).ok_or_else(|| {
            CoreError::from(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("handler '{name}' not found"),
            ))
        })?;
        self.run(&src).await
    }
}

/// 启动 DevTools inspector：借一个 runtime 取其 inspector 句柄并起 WS 服务。
/// 仅当 `inspect=true` 构造时有效；addr 形如 `127.0.0.1:9229`。
pub fn start_inspector(bridge: &Bridge, addr: std::net::SocketAddr) {
    if !bridge.inspect {
        tracing::warn!(target: "inspector", "inspector not enabled at construction");
        return;
    }
    let rt = bridge.pool.checkout();
    let insp = rt.inspector();
    runtime::RuntimePool::checkin(&bridge.pool, rt);
    inspector::spawn(insp, addr);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn new_bridge() -> (Bridge, Arc<InMemoryAccessor>) {
        let db = Arc::new(InMemoryAccessor::new());
        db.seed([json!({"id": 1, "name": "ever"})]);
        let b = Bridge::new(db.clone(), Arc::new(InMemoryKV::new()));
        (b, db)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn globals_injected_and_json_ok() {
        let (b, _) = new_bridge();
        let cap = b
            .run_with(
                r#"
                const injected = typeof json === "object" && typeof db === "object"
                    && typeof http === "object" && typeof redis === "object"
                    && typeof DB === "function" && typeof fetch === "function"
                    && typeof log === "object" && typeof finish === "function";
                log.info("handler start", "method", http.method);
                json.header("X-Handler", "test");
                json.ok({ injected, method: http.method });
                "#,
                RequestInfo {
                    method: "POST".into(),
                    query: [("id".into(), "1".into())].into_iter().collect(),
                    body: br#"{"a":1}"#.to_vec(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(cap.status, 200);
        assert_eq!(cap.headers.get("X-Handler").unwrap(), "test");
        let v: Value = serde_json::from_slice(&cap.body).unwrap();
        assert_eq!(
            v,
            json!({"code": 0, "msg": "ok", "data": {"injected": true, "method": "POST"}})
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn db_params_and_build() {
        let db = Arc::new(InMemoryAccessor::new());
        db.seed([json!({"id": 1, "name": "ever"})]);
        let registry = SchemaRegistry::new().table("user", Some("id"), &["id", "name", "age"]);
        let b = Bridge::with_opts(db, Arc::new(InMemoryKV::new()), registry, false);
        // 结构化查询构造器（经 SchemaRegistry 白名单）。
        let cap = b
            .run(
                r#"
                db.table("user").select(["id","name"]).where({field:"id",op:"eq",value:1})
                  .orderBy([{field:"id",dir:"asc"}]).limit(10).all()
                  .then((rows) => json.ok({ rows, missing: typeof DB("nope") === "undefined" }))
                  .catch((e) => json.fail(500, String(e)));
                "#,
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&cap.body).unwrap();
        assert_eq!(v["code"], 0, "handler failed: {v}");
        assert_eq!(v["data"]["rows"], json!([{"id": 1, "name": "ever"}]));
        assert_eq!(v["data"]["missing"], true);

        // 原始 SQL + 绑定参数（无拼接）。
        let cap = b
            .run(r#"db.query("select * from user where id = $1", [1]).then((rows) => json.ok({ rows })).catch((e) => json.fail(500, String(e)));"#)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&cap.body).unwrap();
        assert_eq!(v["code"], 0, "param query failed: {v}");
        assert_eq!(v["data"]["rows"], json!([{"id": 1, "name": "ever"}]));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unknown_table_rejected() {
        let (b, _) = new_bridge();
        let cap = b
            .run(r#"db.table("secret").select(["x"]).all().then((r) => json.ok(r)).catch((e) => json.fail(400, String(e)));"#)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&cap.body).unwrap();
        assert_eq!(v["code"], 400);
        assert!(v["msg"].as_str().unwrap().contains("unknown table"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pool_reuses_runtime_isolation() {
        // 两次请求相继执行，验证每请求状态被正确重置（req 不串号）。
        let (b, _) = new_bridge();
        let cap1 = b
            .run_with(r#"json.ok({ m: http.method });"#, RequestInfo { method: "GET".into(), ..Default::default() })
            .await
            .unwrap();
        let cap2 = b
            .run_with(r#"json.ok({ m: http.method });"#, RequestInfo { method: "PUT".into(), ..Default::default() })
            .await
            .unwrap();
        let v1: Value = serde_json::from_slice(&cap1.body).unwrap();
        let v2: Value = serde_json::from_slice(&cap2.body).unwrap();
        assert_eq!(v1["data"]["m"], "GET");
        assert_eq!(v2["data"]["m"], "PUT");
    }
}
