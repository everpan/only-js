# Go(goja) vs Rust(deno_core) 性能对比

同机同口径实测：Apple M5 / arm64，2026-08-02。
- Go：`mdm-base/internal/bridge/bridge_bench_test.go`，`go test -bench=BenchmarkBridge`
- Rust：`benches/bridge.rs`，`cargo bench`（数字取自 docs/benchmarks.md 优化后结果）

## 方法学与差异

两版均分两层：纯宿主语言层单次调用；JS 绑定全链路（每次迭代 JS 侧执行 100 次 op，
fetch 20 次，除以次数得单次）。不可消除的口径差异：

- goja 程序预编译一次复用；Rust `execute_script` 每次迭代编译（V8 有代码缓存摊薄）。
- goja 无 async/await，异步 op 用 Promise 链串行；Rust 用 async IIFE + await。
- Go fetch 每次调用 `fiber client.New()`（无连接池）；Rust 共享 reqwest Client（keep-alive）。
- Go 日志为 zap JSON 编码写 `io.Discard`；Rust 为 tracing fmt 写 `io::sink`（均含格式化）。

## 结果

| 基准 | Go(goja) 单次 | Rust(deno_core) 单次 | Rust 加速比 |
|---|---:|---:|---:|
| 纯宿主层 envelope.ok | 727 ns（20 allocs） | 70 ns | **10.4x** |
| 纯宿主层 kv.get | 6.9 ns | 76 ns | 0.09x（Go 快） |
| 纯宿主层 kv.set | 42.9 ns | 80 ns | 0.54x |
| 纯宿主层 accessor.query | 11.2 ns | 145 ns | 0.08x（Go 快） |
| 每请求固定开销（baseline） | 6.40 µs | 515 ns | **12.4x** |
| js/json.ok | 1.36 µs | 230 ns | **5.9x** |
| js/log.info | 1.08 µs | 611 ns | **1.8x** |
| js/redis.get | 4.75 µs | 138 ns | **34x** |
| js/redis.set | 4.87 µs | 155 ns | **31x** |
| js/db.query | 5.03 µs | 413 ns | **12x** |
| js/db.exec | 4.71 µs | 146 ns | **32x** |
| js/fetch(local) | 219.6 µs | 29.7 µs | **7.4x** |

换算吞吐（js 层）：redis.get Go 21 万/s vs Rust 726 万/s；db.query Go 20 万/s vs
Rust 242 万/s；fetch Go 4.6 K/s vs Rust 33.7 K/s。

## 解读

- **异步 op 是最大差距（12–34x）**。Go 版每个异步 op = 一次 goroutine 生成 +
  eventloop `RunOnLoop` 通道往返 + goja 解释执行 Promise 链回调；deno_core 的
  `#[op2] async fn` 由 V8 原生 Promise + tokio 驱动，单次边界仅 ~140 ns。
- **每请求固定开销（12.4x）**。Go 每个请求 eventloop Start/Stop（goroutine 生命周期）
  + 全量 Apply；Rust 创建 Isolate 后复用，`run()` 固定成本 0.5 µs。
- **纯宿主层 Go 反而快**的是 fake 实现细节（sync.Map 无锁读、浅拷贝行切片、无字符串
  分配），非产品路径——真实 Redis/SQL 接入后此项无关。
- **fetch（7.4x）** 含实现差异：Go 版每请求新建 fiber client（无连接复用），
  Rust 版共享连接池。若 Go 版改共享 client 差距会缩小，但 op 边界差距不变。
- **log（1.8x）** 差距最小：两边成本都在结构化日志格式化，引擎差异被摊薄。

## 复现

```bash
# Go
cd ../mdm-base && go test ./internal/bridge/ -run=^$ -bench=BenchmarkBridge -benchtime=1s
# Rust
cargo bench
```
