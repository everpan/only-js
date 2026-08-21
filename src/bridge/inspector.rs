//! InspectorServer：基于 deno_core 自带的 `JsRuntimeInspector` 提供 Chrome DevTools 调试。
//!
//! 不依赖 `deno_runtime`：deno_core 已暴露 `JsRuntimeInspector::create_local_session`，
//! 本模块补上 CDP 的 WebSocket 传输（tungstenite）。每条 WS 连接对应一个 local session，
//! 双向转发 DevTools 前端与 V8 inspector 之间的 JSON 消息。

use std::net::SocketAddr;
use std::rc::Rc;

use deno_core::{InspectorMsg, InspectorSessionKind, JsRuntimeInspector};
use futures::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::accept_async;

/// 启动一个 inspector WebSocket 服务，监听 `addr`，将连接绑定到给定 runtime 的 inspector。
///
/// 该 runtime 必须以 `RuntimeOptions { inspector: true }` 创建。服务在后台 task 运行。
pub fn spawn(inspector: Rc<JsRuntimeInspector>, addr: SocketAddr) {
    tokio::task::spawn_local(async move {
        let listener = match TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(target: "inspector", %e, "bind failed");
                return;
            }
        };
        tracing::info!(target: "inspector", %addr, "DevTools inspector listening (chrome://inspect)");
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            let ws = match accept_async(stream).await {
                Ok(w) => w,
                Err(e) => {
                    tracing::warn!(target: "inspector", %e, peer = %peer, "ws handshake failed");
                    continue;
                }
            };
            let insp = inspector.clone();
            tokio::task::spawn_local(session_loop(insp, ws));
        }
    });
}

/// 单条 DevTools 连接的消息桥接：WS 文本 <-> local session。
async fn session_loop(inspector: Rc<JsRuntimeInspector>, ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) {
    let (mut ws_tx, mut ws_rx) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    // 出站消息经 tx 推给 WS。
    let mut sess = JsRuntimeInspector::create_local_session(
        inspector,
        Box::new(move |msg: InspectorMsg| {
            let _ = tx.send(msg.content);
        }) as Box<dyn Fn(InspectorMsg)>,
        InspectorSessionKind::Blocking,
    );

    // 出站：rx -> WS。
    let pump = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_tx
                .send(tokio_tungstenite::tungstenite::Message::Text(msg.into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    // 入站：WS -> dispatch。
    while let Some(Ok(msg)) = ws_rx.next().await {
        match msg {
            tokio_tungstenite::tungstenite::Message::Text(t) => {
                sess.dispatch(t.to_string());
            }
            tokio_tungstenite::tungstenite::Message::Close(_) => break,
            _ => continue,
        }
    }
    pump.abort();
}
