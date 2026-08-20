# bridge —— deno_core JS SDK 桥接模块

移植自 Go 版 `mdm-base/internal/bridge`（goja → deno_core 0.409），向 V8 JsRuntime 注入
JS SDK 全局对象，供 JS handler 编写业务逻辑，Rust 侧捕获统一信封响应。

## 架构

```
┌─────────────── JS handler ───────────────┐
│  json / db / DB / http / redis / log     │
│  / fetch / finish   (bootstrap.js 装配)  │
└───────────────── op 调用 ────────────────┘
                   │ #[op2]
┌──────────────────┴───────────────────────┐
│  ops (json/db/kv/fetch/http/log/query)   │
│   ├─ StableState (Arc, 跨请求共享)        │
│   │     kv / dbs / client / registry      │
│   └─ ReqState  (OpState 内, 每请求 reset)  │
│  DataAccessor / KVStore 契约（trait）     │
└────────────────────┬──────────────────────┘
                     │ RuntimePool（复用 JsRuntime）
              checkout → reset(req) → run → checkin
```

- **状态分片**：`StableState` 跨请求不可变（内部句柄均为 `Arc`），`ReqState` 每请求可变，
  存于 `OpState`，`run_with` 借出 runtime 时整体 `reset(req)`。两者分离使 `JsRuntime` 可池化复用。
- **RuntimePool**：复用 `JsRuntime`（V8 isolate 1~10ms 开销），idle 上限 `DEFAULT_MAX_IDLE=16`。
  "快照"等价于已加载 `bootstrap.js` 的预热 runtime——bootstrap 仅编译一次，后续请求只执行 handler 源码。
  **仅成功执行（已轮询 event loop）的 runtime 才归还**；失败的可能 isolate 损坏，直接 drop。
- **异步模型**：Go 版"后台 goroutine + RunOnLoop 回切解析 Promise"由 `#[op2] async fn`
  天然替代——op 直接返回 JS Promise，由 deno_core event loop 驱动，无需手写回切。
- **JS 侧 API 形状**由 `bootstrap.js`（扩展入口）装配，等价于 goja 版的 `map[string]any` 绑定。

## 文件对照

| Rust | Go | 内容 |
|---|---|---|
| `src/bridge/mod.rs` | bridge.go | Bridge 装配骨架、StableState/ReqState、扩展注册、RuntimePool、响应捕获 |
| `src/bridge/envelope.rs` | envelope.go | `{code,msg,data}` 统一信封、状态码映射 |
| `src/bridge/json.rs` | json.go | `json.ok/fail/header` |
| `src/bridge/db.rs` | db.go + accessor.go | `DataAccessor` trait、内存 fake、`db.query/exec`、`DB(name)` |
| `src/bridge/query.rs` | — | 安全查询构造器（sea-query + SchemaRegistry 白名单，参数化值） |
| `src/bridge/registry.rs` | — | `SchemaRegistry` 表/列白名单（SQL 注入根治点） |
| `src/bridge/accessor_sqlx.rs` | — | `SqlxAccessor`：sqlx Any 驱动实现 `DataAccessor` |
| `src/bridge/runtime.rs` | — | `RuntimePool`：复用 JsRuntime（预热=快照等价） |
| `src/bridge/loader.rs` | — | `HandlerStore`：handler 源码加载 + 热重载（FS / 嵌入） |
| `src/bridge/inspector.rs` | — | DevTools inspector WS 桥（deno_core 自带 `JsRuntimeInspector`） |
| `src/bridge/fetch.rs` | fetch.go | `fetch(url, options)`（fiber.Client → reqwest） |
| `src/bridge/http.rs` | http.go | `http.*` 只读请求上下文 |
| `src/bridge/kv.rs` | kv.go | `KVStore` trait、内存实现、`redis.get/set` |
| `src/bridge/log.rs` | log.go | `log.debug/info/warn/error`（zap → tracing） |
| `src/bridge/bootstrap.js` | bridge.go `Apply` | JS 全局对象装配、`DB(name)` 引用缓存、Response 组装、查询构造器 |

## JS API

### json —— 统一信封

```js
json.ok(data)              // {code:0, msg:"ok", data} → status 200，标记会话完成
json.fail(code, msg, data) // code<=0 映射 500
json.header(name, value)   // 设置返回头（覆盖语义），空名忽略
```

信封在 Rust 侧单遍序列化（不构中间 `Value` 树，直接写 buffer），marshal 成本约 70 ns。

### db / DB(name) —— 数据访问（Promise）

```js
// 原始 SQL + 绑定参数（参数防注入，推荐）。
const rows = await db.query("select * from user where id = $1", [1]); // Row[]（JSON 对象数组）
const n = await db.exec("update user set age = $1 where id = $2", [19, 1]); // 受影响行数

// 安全查询构造器（标识符白名单 + 值参数化，强烈推荐）。
const rows = await db.table("user")
  .select(["id", "name"])
  .where({ field: "age", op: "gte", value: 18 })
  .orderBy([{ field: "id", dir: "desc" }])
  .limit(10).all();

db === DB("default")                               // true（JS 侧缓存保证引用相等）
DB("reports")                                      // 命名实例；未配置返回 undefined
```

参数占位符随底层驱动而定：`$1` 用于 Postgres，`?` 用于 MySQL/SQLite（sea-query 构造器固定生成
Postgres 风格 `$N`，接入非 PG 驱动时需转换占位符，见 accessor_sqlx.rs 注释）。

### http —— 请求上下文（只读快照）

```js
http.method  // string
http.params  // {} 路径参数
http.query   // {} 查询参数
http.headers // {} 请求头
http.body    // 能解析为 JSON 则为对象/数组，否则为字符串；空为 null
```

### redis —— M0 内存 KV（Promise）

```js
await redis.set("k", "v");   // true
const v = await redis.get("k"); // string | null
```

### log —— 结构化日志（msg + 交替键值对，同 zap SugaredLogger）

```js
log.info("user login", "user_id", uid, "ip", ip);
log.error("db query failed", "sql", sql, "err", err);
```

### fetch —— 浏览器 Fetch API 兼容（reqwest 实现）

```js
const resp = await fetch("https://api.example.com/data", {
  method: "POST", headers: { "X-A": "1" }, body: "ping",
});
resp.ok; resp.status; resp.statusText; resp.headers;
const data = await resp.json();   // 空 body → null
const text = await resp.text();
const buf  = await resp.arrayBuffer(); // Uint8Array
const r2   = resp.clone();
const { done, value } = await resp.body.getReader().read(); // 缓冲模拟：首读全量，再读 done
```

未设置 Content-Type 且有 body 时自动补 `text/plain;charset=UTF-8`。不支持 AbortController（同 Go 版）。
HTTP 客户端为单个共享 reqwest Client（连接池复用），`no_proxy`——不走系统代理，对齐
Go fiber client 行为；需要代理支持时按配置注入 `Proxy`（见 `src/bridge/mod.rs` 注释）。

> 注意：reqwest 默认会读取 macOS 系统代理配置，本机有代理软件时连回环地址都会被
> 拦截转发（实测 65 µs/req 劣化到 1.9 ms/req）。这是 `no_proxy` 的直接原因。

### finish —— 标记会话完成

```js
finish(); // 等价于 json.ok/fail 的 SignalDone 语义，但不写响应
```

## Rust API

```rust
use std::sync::Arc;
use mdm_base_rust::bridge::{
    Bridge, InMemoryAccessor, InMemoryKV, RequestInfo, SchemaRegistry,
};

let db = Arc::new(InMemoryAccessor::new());
db.seed([serde_json::json!({"id": 1})]);
let kv = Arc::new(InMemoryKV::new());

// schema 注册表：动态标识符白名单（SQL 注入根治点）。仅在构造时声明。
let registry = SchemaRegistry::new()
    .table("user", Some("id"), &["id", "name", "age"]);

// 含注册表 + inspector 开关；inspect=true 时运行时启用 DevTools inspector。
let mut b = Bridge::with_opts(db, kv, registry, false);

// 命名实例（须在首次 checkout runtime 之前调用；StableState 一旦共享即不可变）。
b.set_db_accessors([("reports".into(), reports_da)]);

// 注入请求上下文并执行；run() 等价于 run_with(src, RequestInfo::default())。
let cap = b.run_with(handler_js_source, RequestInfo {
    method: "GET".into(),
    query: [("id".into(), "1".into())].into_iter().collect(),
    ..Default::default()
}).await?;

// 捕获响应由 run_with 直接返回，无需再取。
let Capture { status, headers, body } = cap;

// 按名执行已加载的 handler（生产用嵌入 map，开发用 MDM_HANDLER_DIR 热重载）。
let cap = b.run_named("user.get").await?;
```

`Bridge::new(db, kv)` 是 `with_opts` 的便捷形式：默认空注册表、`inspect=false`、并将传入 db 注册为 `"default"`。

### 契约（依赖倒置）

```rust
#[async_trait]
pub trait DataAccessor: Send + Sync {
    async fn query(&self, sql: &str) -> BridgeResult<Vec<Row>> { ... }        // 默认转 *_with_params
    async fn exec(&self, sql: &str) -> BridgeResult<i64> { ... }             // 默认转 *_with_params
    async fn query_with_params(&self, sql: &str, params: &[Value]) -> BridgeResult<Vec<Row>>;
    async fn exec_with_params(&self, sql: &str, params: &[Value]) -> BridgeResult<i64>;
}

#[async_trait]
pub trait KVStore: Send + Sync {                   // 后续真实 Redis 以同接口接入
    async fn get(&self, key: &str) -> BridgeResult<Option<String>>;
    async fn set(&self, key: &str, value: &str) -> BridgeResult<()>;
}
```

`BridgeResult<T> = Result<T, Box<dyn Error + Send + Sync>>`，op 层转为 `JsErrorBox` 抛给 JS。
`SqlxAccessor`（accessor_sqlx.rs）已实现 `DataAccessor`，以 `Pool<Any>` 驱动无关接入真实 MySQL/PG/SQLite。

## 与 Go 版的差异

- **Promise**：goja 需手工 NewPromise + RunOnLoop 回切；deno_core async op 直接返回 Promise。
- **错误**：op 错误类型用 `deno_error::JsErrorBox`（anyhow 不再被 op2 接受）。
- **per-request 状态**：`ReqState`（请求上下文 + 响应捕获）存入 `OpState`，每次 `run_with`
  借出 runtime 时 `reset(req)`；`StableState`（kv/dbs/client/registry）为 `Arc` 跨请求共享。
  故 `Bridge` 可池化复用 `JsRuntime`（`RuntimePool`），无需每请求新建 isolate。
- **未移植**：`ws`（Go 版即占位）、`Redis(name)` 真实实例、`XORM(name)` ORM——server 层
  需要时按 `DB(name)` 既有模式接入 redis/sqlx。

## 测试与性能

```bash
cargo test        # 7 个单元测试（含本地 HTTP 服务器的 fetch 全链路、query 构造器、白名单校验）
cargo test --doc  # 文档测试（deno_core 扩展宏自带的 doctest 为 ignored）
cargo build --benches && cargo bench   # criterion 性能测试（含吞吐统计），见 docs/benchmarks.md
cargo run         # 演示：构造 Bridge、注入请求、跑业务 JS、读捕获响应
```

当前量级（Apple Silicon，release）：异步 op 边界约 140 ns（redis/db.exec 7 M/s 级），
json.ok 230 ns，db.query 413 ns，fetch 本地回环复用连接 30 µs/req（33.7 K/s）。
详见 [benchmarks.md](benchmarks.md)（含优化前后对比与根因分析）。

## deno_core 0.409 移植要点

- `#[op2]` 无 `(async)` 标志，`async fn` 自动识别。
- `#[serde]` 位置须写全限定 `serde_json::Value`；`Option<String>` 参数用 `#[string]`。
- 扩展 JS 必须 7-bit ASCII；`esm_entry_point` specifier 为 `ext:<扩展宏名>/<file>`。
- 同步 op 第一参 `&mut OpState`，异步 op 第一参 `Rc<RefCell<OpState>>`。
