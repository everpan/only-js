use std::sync::Arc;

use mdm_base_rust::bridge::{
    Bridge, InMemoryAccessor, InMemoryKV, RequestInfo, SchemaRegistry,
};

/// 演示：构造 Bridge（含 schema 白名单 + runtime 池 + 可选 inspector），跑业务 JS、
/// 读捕获响应。等价于 Go 版 bridge_test.go 的 runScript。
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let db = Arc::new(InMemoryAccessor::new());
    db.seed([serde_json::json!({"id": 1, "name": "ever", "age": 18})]);
    let kv = Arc::new(InMemoryKV::new());

    // schema 注册表：动态标识符白名单（SQL 注入根治点）。
    let registry = SchemaRegistry::new().table("user", Some("id"), &["id", "name", "age"]);

    // inspect=true 时运行时启用 DevTools inspector（需 start_inspector 起 WS 服务）。
    let inspect = std::env::var("MDM_INSPECT").is_ok();
    let b = Bridge::with_opts(db, kv, registry, inspect);
    // b.warm(2); // 预热 runtime 池

    if inspect {
        mdm_base_rust::bridge::start_inspector(
            &b,
            "127.0.0.1:9229".parse().unwrap(),
        );
    }

    let cap = b
        .run_with(
            r#"
            redis.set("last_query", "user")
              .then(() => db.table("user")
                .select(["id","name"])
                .where({field:"age",op:"gte",value:18})
                .orderBy([{field:"id",dir:"desc"}])
                .limit(10).all())
              .then((rows) => {
                json.header("X-Handler", "demo");
                json.ok({ users: rows, req: { method: http.method, query: http.query } });
              })
              .catch((e) => json.fail(500, String(e)));
            "#,
            RequestInfo {
                method: "GET".into(),
                query: [("id".into(), "1".into())].into_iter().collect(),
                ..Default::default()
            },
        )
        .await?;

    println!("status={} headers={:?}", cap.status, cap.headers);
    println!("{}", String::from_utf8_lossy(&cap.body));
    Ok(())
}
