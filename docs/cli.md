# Rust 版 CLI 实现预案（可行性分析 + 扩充版）

## 0. 文档性质与用法

本文件是 **working plan（边做边更新的活文档）**，不是 final design。每完成一个 phase 后，
在该 phase 末尾追加一行「实现后记录」（含 commit hash / 关键取舍 / 遇到的问题），方便后续复盘。
建议一个 phase 拆成一个 git commit（PR 形式），PR 标题写清本 phase 做了什么。

---

## 1. 现状评估（可行性基线）

### 1.1 已验证可用（release）

Rust 端为 **单 crate `mdm-base-rust`**（根目录 `Cargo.toml`，`src/main.rs` 为默认 bin，
`src/lib.rs` 暴露 `bridge` 模块）。**无 `crates/`、`build-deps/` 目录**（下文各处若再出现需纠正）。

- `cargo build` 通过；`cargo test --release --lib` **7 passed, 0 failed**。
- 核心链路已在 Go 参考（`~/git/golang/mdm-base/internal/bridge`）对齐处实现：
  - `runtime.rs` — RuntimePool：复用 JsRuntime，bootstrap 只编译一次，后续请求仅执行 handler 源码。
  - `query.rs` + `registry.rs` — sea-query 动态构建 SELECT，标识符走 SchemaRegistry 白名单，值参数化（SQL 注入根治点）。
  - `db.rs` / `kv.rs` — DataAccessor / KVStore 统一契约 + 内存 fake（Liskov 可替换为 sqlx/redis）。
  - `fetch.rs` — reqwest 实现 fetch，带本地 HTTP 全链路测试。
  - `inspector.rs` — 基于 deno_core 内置 inspector 的 DevTools WS 桥。
  - `loader.rs` — HandlerStore：FS 热重载（notify）+ 嵌入 map。
  - `json.rs` / `envelope.rs` — 单遍序列化信封。

### 1.2 已知缺陷（Phase 0 必须修）

**`cargo test`（debug 配置）当前 5 个测试全挂**，根因单一：

> `Extension code must be 7-bit ASCII: ext:bridge_ext/bootstrap.js (found 由侧为文本传入…)`

deno_core 0.409 对扩展源码做 7-bit ASCII 校验，**debug profile 严格、release profile 放行**
（故 release 测试能过）。`bootstrap.js` 内的中文注释触发了校验。

修复方式（二选一，推荐 A）：
- **A（推荐）：删除 `bootstrap.js` 中的中文注释**，改英文或拼音缩写，使 debug 也通过。
- B：在 `bridge_ext` 里加 build-time 断言显式声明「release 允许非 ASCII」（仅掩盖，不推荐）。

修完后应做到 **`cargo test`（debug）与 `cargo test --release`（release）双绿**。

### 1.3 未移植清单（Go 有、Rust 无）

| Go 实现 | Rust 现状 | 说明 |
|---|---|---|
| `redis.go` `Redis(name)` | 无 | 完整命令集（string/hash/list/set/zset/pubsub），需 redis crate |
| `xorm.go` `XORM(name)` | 无 | 链式 Session API + 原生 SQL 兜底，需 xorm crate |
| `ws.go` `ws.*` WebSocket | 无 | 帧循环 + 注册能力，**全案最大风险**（见 2.4） |
| server 层超时熔断 | 无 | Go 用 `vm.Interrupt` 熔断死循环；deno_core 无直接等价 |
| config 层 | 无 | Go `internal/config`；需 serde_yaml |
| router 层 | 无 | Go `internal/router` 的 `{METHOD}.js` 路径解析 |

### 1.4 结构参考（Go 侧，勿照搬目录）

```
~/git/golang/mdm-base/
├── internal/bridge/   # 对应 src/bridge/  （已移植 80%）
├── internal/runtime/  # Session/Pool      （对应 src/bridge/runtime.rs）
├── internal/server/   # axum/fiber server （待新建 server crate）
├── internal/router/   # 路径解析          （待移植）
├── internal/config/   # 配置              （待移植）
└── cmd/devserver/     # 装配 + 生命周期   （待移植）
```

---

## 2. 分阶段可行性分析

> 风险等级：🟢 低 / 🟡 中 / 🔴 高。前置依赖指本阶段开始前必须完成的阶段。

| Phase | 内容 | 可行性 | 风险 | 前置 | 关键工作量 |
|---|---|---|---|---|---|
| 0 | ASCII 校验 / build 绿 | 高 | 🟢 | — | trivial |
| 1 | sqlx 接入 | 中高 | 🟡 | P0 | 小（`accessor_sqlx.rs` 已存在） |
| 2 | Server 层（axum） | 高 | 🟡 | P1 | 中（新 crate） |
| 3 | config 层 | 高 | 🟢 | — | 小 |
| 4 | Server 生命周期 | 高 | 🟡 | P3+P4-glue | 中 |
| 5 | WebSocket 帧循环 | 中 | 🔴 | P2 | 大（见 2.4） |
| 6 | 性能优化 | 高 | 🟢 | — | 小（多数已实现） |

### 2.1 Phase 0 — ASCII 校验 / build 绿（🟢 trivial）

**目标：** debug + release 测试全绿。
**改动：** 删 `bootstrap.js` 中文注释；跑 `cargo test` 与 `cargo test --release` 确认双绿。
**验证：** 两配置均 7 passed。

### 2.2 Phase 1 — sqlx 接入（🟡 中）

**目标：** 把 `SqlxAccessor`（`src/bridge/accessor_sqlx.rs`，**已实现**）接入 `Bridge`，替换内存 fake。

**关键事实与风险：**
- `accessor_sqlx.rs` 已满足 `DataAccessor` trait（`query_with_params`/`exec_with_params`，Any 驱动）。
- **sqlx 需注册 runtime handler**：连接前调用 `sqlx::runtime::set_handler(&sqlx::runtime::Handler::tokio())`
  （feature `runtime-tokio` 已开）。deno_core 跑 current_thread tokio，`Handler::tokio()` 兼容。
  ⚠️ 此调用必须在**任何 `Pool::connect` 之前**全局调用一次（main 入口或 lib init）。
- 通过 `Bridge::set_db_accessors`（已存在，注释「须在首次 checkout 之前调用」）注入命名实例。

**改动：**
1. main 入口调 `sqlx::runtime::set_handler(...)`。
2. 用 `SqlxAccessor::arc(dsn)` 构造 `Arc<dyn DataAccessor>`，`set_db_accessors` 注入 `default`（及命名实例）。
3. 补一个连 sqlite memory/temp 的集成测试（`#[tokio::test]`），验证 `db.query`/`db.table` 真实落库。

### 2.3 Phase 2 — Server 层（axum，🟡 中，新 crate）

**目标：** 新建 `server/` crate，实现 `GET/POST/.../*.js` 路由解析 + 统一信封输出。

**关键事实与风险：**
- **新 crate = 独立 `server/Cargo.toml`**，`[dependencies]` 依赖 `mdm-base-rust`（workspace member）。
- 工作区 edition 2024；axum 0.8 支持 edition 2024（确认版本兼容后再定）。
- **HTTP 路径无 `!Send` 风险**：`Bridge`/`JsRuntime` 在单个请求 task 内创建+使用+丢弃，不跨线程。
  这与 Phase 5（WS 跨帧复用同一 runtime）有本质区别——**Phase 2 安全，Phase 5 是难点**。
- router 逻辑直接移植 Go `internal/router/Resolve`（`{mode}-{version}/{sub}/{feature}/{entity}/{METHOD}.js`）。

**改动：**
1. `server/` crate + `src/lib.rs`（`Server` 结构、`Router` 解析、`handle`）。
2. `handle`：resolve → `Bridge::run_named(method)` → 写 `capture.status`/`headers`/`body`。
3. 移植 Go 的 404/compile-error 信封语义。

### 2.4 Phase 3 — config 层（🟢 小）

**目标：** 移植 Go `internal/config`（`Config`/`ServerConfig`/`DBConfig`/`RedisConfig`）。
**技术选型：** `serde_yaml` 0.9（已 deprecated，功能可用；若团队抵触可换 `serde_yml` fork）。
**风险：** deprecated crate 无安全更新——仅内部配置解析，可接受；在 `Cargo.toml` 注释标注。

### 2.5 Phase 4 — Server 生命周期（🟡 中）

**目标：** 装配 config → sqlx pool → Bridge → server，含命名 DB 实例构建。
**改动：** main 读 config → 每个 `db.<name>` 开一个 sqlx `Pool<Any>` → `SqlxAccessor::from_pool` →
`Bridge::set_db_accessors` → `Server::new`。
**风险：** sqlx Any 多驱动（mysql/postgres/sqlite）的连接串格式差异；先只做 sqlite 跑通，再扩。

### 2.6 Phase 5 — WebSocket 帧循环（🔴 高，全案最大风险）

**目标：** 移植 Go `ws.go` 的 `ws.*` 绑定 + 三协程帧循环。

**为什么是最大风险 —— 核心矛盾：**
- deno_core 的 `JsRuntime` 是 **`!Send`**（不可跨线程）且**绑定 current_thread tokio**（见 `runtime.rs` 注释）。
- axum 的 WebSocket 默认在 **hyper 多线程 runtime** 上跑 handler，帧可能在不同线程被拾取 → **V8 isolate 跨线程 = 直接崩**。
- Go 用 goroutine（天然跨线程，goja 线程不安全所以它把 VM 操作锁回 loop 协程）；Rust 没有等价物，
  **必须把整条帧循环钉在单个 current_thread 句柄内**（`tokio::Runtime::block_in_place` 或
  专用 `Builder::new_current_thread().build()` + 该 handle 上 `spawn`）。

**推荐架构（重构自 Go 三协程）：**
- 每个 WS 连接一个**当前线程的 runtime 工作单元**（从 RuntimePool checkout 一个 JsRuntime，绑定到该连接的生命周期）。
- 用 3 个 `tokio::task`（在**同一个 current_thread handle** 上 spawn）：Reader（读帧 → mpsc）/
  Processor（`Bridge::run` → 出响应 → mpsc）/ Writer（消费响应 → `Upgrade` 写回）。
- msgChan/respChan 各 `mpsc::bounded(64)` 提供背压。
- ⚠️ **不要**让 axum 的 WS handler 直接 `await` 一个可能跨帧持有 JsRuntime 的 future——
  必须用上面的"钉死在 single-thread handle"模式。

**建议分两步降低风险：**
1. **5a（MVP）：** 先做一个最小 WS echo（不执行 JS handler，仅透传帧），先打通 axum+upgrade+single-thread pin 这条最危险的链路。
2. **5b：** 再叠加 JS handler 帧循环。
若 5a 的 pin 模式验证通过，5b 只是逻辑填充。

### 2.7 Phase 6 — 性能优化（🟢 小，多数已实现）

**目标：** 扩 fetch 能力（signal/AbortController）、代码缓存。
**关键事实：** 池化复用（`runtime.rs`）、单遍信封序列化（`envelope.rs`）、`serde_v8` 规避（`json.rs` fast op）
**均已实现**。fetch 已可用（`fetch.rs`），仅缺 signal/AbortController（同 Go 版限制）。
**改动：** fetch 加 optional `signal`；handler 源码走 `execute_script_with_cache`（V8 代码缓存）。

---

## 3. 扩充后的实施顺序（依赖图）

```
P0 (build 绿)
  │
  ├─→ P1 (sqlx) ──→ P2 (axum server) ──→ P5a (WS 最小链路) ──→ P5b (WS JS 帧循环)
  │                                                 │
  └─→ P3 (config) ──→ P4 (生命周期装配) ──────────────┘
                        │
                        └──────────────────────→ P6 (perf, 可并行)
```

- **关键路径：** P0 → P1 → P2 → P5a → P5b。
- P3/P4 可早开始（不依赖 P2）。
- P6 与 P5b 后段可并行。
- **Phase 5 是成败关键**：建议在 P2 一过就立即做 P5a 探路，把 `!Send` pin 风险在早期暴露，
  而非留到最后。

---

## 4. 风险与对策

| 风险 | 影响 | 对策 |
|---|---|---|
| deno_core `!Send` + axum 多线程 WS | V8 isolate 跨线程崩 | P5a 先验证 single-thread pin 模式；帧循环钉在单个 current_thread handle |
| sqlx runtime handler 时序 | connect 失败/panic | main 入口尽早 `set_handler`，连库前 |
| edition 2024 × axum/依赖兼容 | 编译失败 | P2 前先 `cargo add` 冒烟，确认版本 |
| serde_yaml deprecated | 无安全更新 | 内部配置解析，可接受；注释标注，预留换 `serde_yml` |
| 命名 DB 多驱动连接串 | 仅 sqlite 跑通 | 先 sqlite memory，再扩 pg/mysql |
| debug/release ASCII 行为不一致 | 测试口径错乱 | 统一删中文注释，双配置都绿 |

---

## 5. 测试计划（对照 Go `_test.go`）

每阶段补测试，目标覆盖 Go 侧已有断言：

- **P0：** debug/release 双绿（回归现有 7 测）。
- **P1：** sqlite 集成测（`db.query`/`db.table` 真实落库 + 绑定参数 + 未知表拒绝）。
- **P2：** router 解析测（成功/缺文件/无版本段/段数不足/extra rest）——直接移植 Go `router_test.go`。
- **P3：** config 加载测（默认值/env 覆盖/base_dir 回落）——移植 `config_test.go`。
- **P5：** WS echo 测（连接→收发→关闭），验证帧循环不回主线程。
- 端到端：`GET /crm-v1/user/profile/list` → 返回统一信封（对齐 Go `bridge_test.go`）。

---

## 6. 实现后记录（填 commit hash / 取舍 / 问题）

- **[P0]** commit `6d9166b` 之后：删 `bootstrap.js` 中文注释（改英文），根因 deno_core 0.409
  的 7-bit ASCII 校验 debug 严格、release 放行。取舍：放弃"release 允许中文"，改源头清 ASCII，
  避免 debug/release 行为分裂。`cargo test` 与 `--release` 均 7 passed。
- …（后续每 phase 追加一行）
