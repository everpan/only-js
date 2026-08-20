//! RuntimePool：复用 JsRuntime 实例，避免每请求新建 V8 isolate（1~10ms 开销）。
//!
//! 每个 pooled runtime 在创建时即加载 bridge_ext（含 bootstrap.js ESM 入口），故"快照"等价于
//! 一次预热后反复复用——bootstrap 只编译一次，后续请求仅执行 handler 源码。
//! 配合 `execute_script_with_cache` 可对 handler 源码做 V8 代码缓存，进一步摊薄编译成本。
//!
//! 由于 `JsRuntime` 是 `!Send`，池与持有它的 event loop 同线程（当前为 tokio current_thread），
//! 与现有 per-request `Bridge` 模型一致。跨请求的状态隔离由 `ReqState` 在 checkout 时重置保证。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use deno_core::{JsRuntime, PollEventLoopOptions, RuntimeOptions};

use super::{StableState, bridge_ext};

/// 池容量上限（空闲实例数）。设为 0 表示无上限（按需增长后保留）。
const DEFAULT_MAX_IDLE: usize = 16;

/// JsRuntime 池。
pub struct RuntimePool {
    /// 构造新 runtime 的工厂（捕获共享的 StableState）。
    make: Box<dyn Fn() -> JsRuntime>,
    /// 空闲 runtime 列表。
    idle: RefCell<Vec<JsRuntime>>,
    /// 空闲上限。
    max_idle: usize,
}

impl RuntimePool {
    /// 用共享的稳定状态构造池。inspect=是否启用 DevTools inspector。
    pub fn new(stable: Arc<StableState>, inspect: bool) -> Self {
        Self {
            make: Box::new(move || {
                JsRuntime::new(RuntimeOptions {
                    extensions: vec![bridge_ext::init(stable.clone())],
                    inspector: inspect,
                    ..Default::default()
                })
            }),
            idle: RefCell::new(Vec::new()),
            max_idle: DEFAULT_MAX_IDLE,
        }
    }

    /// 借出一个 runtime（优先复用空闲，否则新建）。
    pub fn checkout(&self) -> JsRuntime {
        if let Some(rt) = self.idle.borrow_mut().pop() {
            return rt;
        }
        (self.make)()
    }

    /// 归还一个 runtime 到空闲池（超出上限则丢弃，由 drop 析构 V8 isolate）。
    /// 仅归还已成功执行过 event loop 的 runtime（未轮询的 isolate 析构会触发 V8 句柄错误）。
    pub fn checkin(&self, rt: JsRuntime) {
        let mut idle = self.idle.borrow_mut();
        if idle.len() < self.max_idle {
            idle.push(rt);
        }
    }
}

/// 在给定 runtime 上执行脚本并驱动 event loop 至所有 Promise 落定。
/// "快照/预热"由 RuntimePool 复用已加载 bootstrap 的 runtime 实现；此处仅执行 handler 源码。
pub async fn run_to_completion(
    rt: &mut JsRuntime,
    name: &'static str,
    source: String,
) -> Result<(), deno_core::error::CoreError> {
    rt.execute_script(name, source)?;
    rt.run_event_loop(PollEventLoopOptions::default()).await?;
    Ok(())
}

/// 取 runtime 的 OpState 句柄（用于 checkout 时重置 per-request 状态）。
pub fn op_state(rt: &JsRuntime) -> Rc<RefCell<deno_core::OpState>> {
    rt.op_state()
}
