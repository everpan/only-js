//! JS actor：把 !Send 的 Bridge 钉在专用 OS 线程上，axum 侧只经 channel 通信（future 天然 Send）。
//! actor 线程内跑 current_thread tokio runtime，串行执行 job；请求并发度 = actor 数（P4 按 PoolSize 开多个）。

use mdm_base_rust::bridge::{Bridge, Capture, RequestInfo};
use tokio::sync::{mpsc, oneshot};

/// 一次 handler 执行请求。
pub struct Job {
    pub source: String,
    pub req: RequestInfo,
    pub resp: oneshot::Sender<Result<Capture, String>>,
}

/// JS actor 句柄（Clone = 同一 actor 队列的引用；Send + Sync，可入 axum state）。
#[derive(Clone)]
pub struct JsActor {
    tx: mpsc::Sender<Job>,
}

impl JsActor {
    /// 在专用线程上以工厂构造 Bridge（Bridge !Send，不可跨线程搬预构实例）并开始接收 job。
    pub fn new(make_bridge: impl Fn() -> Bridge + Send + 'static) -> Self {
        let (tx, mut rx) = mpsc::channel::<Job>(64);
        std::thread::Builder::new()
            .name("js-actor".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("actor runtime init");
                let bridge = make_bridge();
                rt.block_on(async move {
                    while let Some(job) = rx.recv().await {
                        let out = bridge
                            .run_with(&job.source, job.req)
                            .await
                            .map_err(|e| e.to_string());
                        let _ = job.resp.send(out);
                    }
                });
            })
            .expect("spawn js-actor thread");
        Self { tx }
    }

    /// 提交执行并等待 Capture（调用方可在任意 tokio runtime 上 await）。
    pub async fn run(
        &self,
        source: impl Into<String>,
        req: RequestInfo,
    ) -> Result<Capture, String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Job {
                source: source.into(),
                req,
                resp: tx,
            })
            .await
            .map_err(|_| "js actor stopped".to_string())?;
        rx.await
            .map_err(|_| "js actor dropped job".to_string())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdm_base_rust::bridge::{InMemoryAccessor, InMemoryKV};
    use serde_json::{Value, json};

    fn actor() -> JsActor {
        JsActor::new(|| {
            Bridge::new(
                std::sync::Arc::new(InMemoryAccessor::new()),
                std::sync::Arc::new(InMemoryKV::new()),
            )
        })
    }

    // 跨线程往返：测试跑在多线程 runtime（同 axum），Bridge 钉在 actor 线程。
    #[tokio::test]
    async fn runs_handler_and_returns_capture() {
        let a = actor();
        let cap = a
            .run(
                r#"json.ok({ m: http.method });"#,
                RequestInfo {
                    method: "GET".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(cap.status, 200);
        let v: Value = serde_json::from_slice(&cap.body).unwrap();
        assert_eq!(v, json!({"code": 0, "msg": "ok", "data": {"m": "GET"}}));
    }

    #[tokio::test]
    async fn reports_js_compile_error() {
        let a = actor();
        let err = a
            .run("this is !!! not valid js", RequestInfo::default())
            .await
            .err()
            .unwrap_or_default();
        assert!(!err.is_empty());
    }
}
