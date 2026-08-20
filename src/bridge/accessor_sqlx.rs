//! SqlxAccessor：以 sqlx（Any 驱动，driver-agnostic）实现 DataAccessor。
//!
//! 与 sea-query 构造器（query.rs）协同：构造器产出参数化 SQL + `Vec<Value>` 参数，
//! 本实现把 `Value` 绑定到 sqlx 语句，并把结果行转回 `serde_json::Value`，匹配 Value 边界。
//! 真实 handler 无需感知底层驱动——`db.query_with_params` / `db.table(...)` 统一经此路径。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use sqlx::any::{Any, AnyArguments};
use sqlx::pool::Pool;
use sqlx::query::Query;
use sqlx::{Column, Row};

use super::{BridgeResult, DataAccessor, Row as JsRow};

/// 基于 sqlx AnyPool 的 DataAccessor 实现。
pub struct SqlxAccessor {
    pool: Pool<Any>,
}

impl SqlxAccessor {
    /// 从连接串构池（sqlite:///path、postgres://..、mysql://..）。
    pub async fn connect(url: &str) -> BridgeResult<Self> {
        let pool = Pool::<Any>::connect(url)
            .await
            .map_err(|e| format!("sqlx connect: {e}"))?;
        Ok(Self { pool })
    }

    /// 包装已有池。
    pub fn from_pool(pool: Pool<Any>) -> Self {
        Self { pool }
    }

    /// 便捷构造 Arc 句柄。
    pub async fn arc(url: &str) -> BridgeResult<Arc<dyn DataAccessor>> {
        Ok(Arc::new(Self::connect(url).await?) as Arc<dyn DataAccessor>)
    }
}

/// 将单个 JSON 值绑定到 sqlx 语句（按类型选择可 Encode 的具体类型）。
fn bind_value<'q>(q: Query<'q, Any, AnyArguments<'q>>, v: &Value) -> Query<'q, Any, AnyArguments<'q>> {
    match v {
        Value::Null => q.bind(None::<String>),
        Value::Bool(b) => q.bind(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                q.bind(i)
            } else if let Some(f) = n.as_f64() {
                q.bind(f)
            } else {
                q.bind(None::<String>)
            }
        }
        Value::String(s) => q.bind(s.clone()),
        other => q.bind(other.to_string()),
    }
}

/// 单行 AnyRow -> serde_json::Value（逐列按类型探测）。
fn row_to_json(row: &sqlx::any::AnyRow) -> Value {
    let mut obj = serde_json::Map::new();
    for col in row.columns() {
        let name = col.name().to_string();
        let ordinal = col.ordinal();
        let val = column_json(row, ordinal).unwrap_or(Value::Null);
        obj.insert(name, val);
    }
    Value::Object(obj)
}

/// 逐列尝试常见类型，首个成功者转 JSON。
fn column_json(row: &sqlx::any::AnyRow, ordinal: usize) -> Option<Value> {
    if let Ok(v) = row.try_get::<Option<bool>, _>(ordinal) {
        return Some(Value::from(v));
    }
    if let Ok(v) = row.try_get::<Option<i64>, _>(ordinal) {
        return Some(match v {
            Some(i) => Value::from(i),
            None => Value::Null,
        });
    }
    if let Ok(v) = row.try_get::<Option<f64>, _>(ordinal) {
        return Some(match v {
            Some(f) => serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null),
            None => Value::Null,
        });
    }
    if let Ok(v) = row.try_get::<Option<String>, _>(ordinal) {
        return Some(match v {
            Some(s) => Value::String(s),
            None => Value::Null,
        });
    }
    if let Ok(v) = row.try_get::<Option<Vec<u8>>, _>(ordinal) {
        return Some(match v {
            Some(b) => Value::String(String::from_utf8_lossy(&b).into_owned()),
            None => Value::Null,
        });
    }
    None
}

#[async_trait]
impl DataAccessor for SqlxAccessor {
    async fn query_with_params(&self, sql: &str, params: &[Value]) -> BridgeResult<Vec<JsRow>> {
        let mut q: Query<'_, Any, AnyArguments<'_>> = sqlx::query(sql);
        for p in params {
            q = bind_value(q, p);
        }
        let rows = q
            .fetch_all(&self.pool)
            .await
            .map_err(|e| format!("sqlx query: {e}"))?;
        Ok(rows.iter().map(row_to_json).collect())
    }

    async fn exec_with_params(&self, sql: &str, params: &[Value]) -> BridgeResult<i64> {
        let mut q: Query<'_, Any, AnyArguments<'_>> = sqlx::query(sql);
        for p in params {
            q = bind_value(q, p);
        }
        let res = q
            .execute(&self.pool)
            .await
            .map_err(|e| format!("sqlx exec: {e}"))?;
        Ok(res.rows_affected() as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 仅类型/构造检查（不连真实库）；保证 SqlxAccessor 满足 DataAccessor trait。
    fn _assert_impl() {
        fn takes<T: DataAccessor>() {}
        takes::<SqlxAccessor>();
    }
}
