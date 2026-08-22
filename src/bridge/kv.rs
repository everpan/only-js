//! redis 全局对象：M0 内置的内存 KV 抽象（移植自 Go kv.go）。
//! 真实 Redis 命名实例 Redis(name) 待 server 层接入 redis crate 时再移植。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::RwLock;

use async_trait::async_trait;
use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use serde_json::Value;

use super::{BridgeResult, StableState};
use std::sync::Arc;

/// Redis 风格键值存储的统一契约（接口隔离）。M0 用内存实现。
#[async_trait]
pub trait KVStore: Send + Sync {
    /// 读取键值，None 表示不存在。
    async fn get(&self, key: &str) -> BridgeResult<Option<String>>;
    /// 写入键值。
    async fn set(&self, key: &str, value: &str) -> BridgeResult<()>;
    /// 删除键（幂等：不存在为成功）。
    async fn del(&self, key: &str) -> BridgeResult<()>;
}

/// KVStore 的内存实现（fake，测试/演示用）。
#[derive(Default)]
pub struct InMemoryKV {
    mu: RwLock<HashMap<String, String>>,
}

impl InMemoryKV {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl KVStore for InMemoryKV {
    async fn get(&self, key: &str) -> BridgeResult<Option<String>> {
        Ok(self.mu.read().unwrap().get(key).cloned())
    }

    async fn set(&self, key: &str, value: &str) -> BridgeResult<()> {
        self.mu
            .write()
            .unwrap()
            .insert(key.to_string(), value.to_string());
        Ok(())
    }

    async fn del(&self, key: &str) -> BridgeResult<()> {
        self.mu.write().unwrap().remove(key);
        Ok(())
    }
}

/// redis.get(key)：Promise<string|null>，不存在为 null。
#[op2]
#[serde]
pub async fn op_kv_get(
    state: Rc<RefCell<OpState>>,
    #[string] key: String,
) -> Result<serde_json::Value, JsErrorBox> {
    let kv = state.borrow().borrow::<Arc<StableState>>().kv.clone();
    match kv.get(&key).await {
        Ok(Some(v)) => Ok(Value::String(v)),
        Ok(None) => Ok(Value::Null),
        Err(e) => Err(JsErrorBox::generic(e.to_string())),
    }
}

/// redis.set(key, value)：Promise<true>。
#[op2]
pub async fn op_kv_set(
    state: Rc<RefCell<OpState>>,
    #[string] key: String,
    #[string] value: String,
) -> Result<bool, JsErrorBox> {
    let kv = state.borrow().borrow::<Arc<StableState>>().kv.clone();
    kv.set(&key, &value)
        .await
        .map_err(|e| JsErrorBox::generic(e.to_string()))?;
    Ok(true)
}

/// kv.del(key)：Promise<true>。
#[op2]
pub async fn op_kv_del(
    state: Rc<RefCell<OpState>>,
    #[string] key: String,
) -> Result<bool, JsErrorBox> {
    let kv = state.borrow().borrow::<Arc<StableState>>().kv.clone();
    kv.del(&key)
        .await
        .map_err(|e| JsErrorBox::generic(e.to_string()))?;
    Ok(true)
}
