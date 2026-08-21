//! devserver：CLI 装配层（移植 Go cmd/devserver/main.go）。
//! parse_args 纯函数 + run_from 全链路装配；bin/devserver.rs 仅是薄壳（bin 无法被测试导入）。
//!
//! 与 Go 的差异：HMR 不单独建 reloader（per-request 读盘 = 免费热重载）；
//! redis 配置暂忽略（M0 内存 KV，warn 提示）。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use mdm_base_rust::bridge::{Bridge, DataAccessor, InMemoryKV, SchemaRegistry, SqlxAccessor};
use mdm_base_rust::config::{self, Config};

use crate::actor::JsActor;

/// 命令行解析（Go parseArgs 等价）：--config/--env/--generate-config。
pub fn parse_args(args: &[String]) -> (String, String, bool) {
    let (mut cfg, mut env, mut generate) = (String::new(), String::new(), false);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--config" if i + 1 < args.len() => {
                cfg = args[i + 1].clone();
                i += 1;
            }
            "--env" if i + 1 < args.len() => {
                env = args[i + 1].clone();
                i += 1;
            }
            "--generate-config" => generate = true,
            _ => {}
        }
        i += 1;
    }
    (cfg, env, generate)
}

/// 解析参数、装配依赖并（listen=true 时）启动监听（cwd 为配置查找目录）。
pub async fn run_with(args: &[String], listen: bool) -> Result<(), String> {
    run_from(Path::new("."), args, listen).await
}

/// 同 run_with，但显式指定配置查找目录（测试用，避免并行测试改进程 cwd）。
pub async fn run_from(dir: &Path, args: &[String], listen: bool) -> Result<(), String> {
    let (config_path, env, generate) = parse_args(args);
    if generate {
        let out = if config_path.is_empty() { "cfg.yml".to_string() } else { config_path };
        config::write_default(&out)?;
        println!("wrote default config to {out}");
        return Ok(());
    }
    let env = if env.is_empty() { std::env::var("APP_ENV").unwrap_or_default() } else { env };
    let cfg = config::load_from(dir, &config_path, &env).map_err(|e| format!("load config: {e}"))?;
    let actor = build_actor(&cfg).await?;
    if !listen {
        return Ok(());
    }
    let timeout = config::parse_duration(&cfg.server.timeout)?;
    let addr = normalize_addr(&cfg.server.addr)?;
    println!(
        "devserver listening on {}  (try GET /crm-v1/user/profile/list)",
        cfg.server.addr
    );
    crate::serve(addr, &cfg.server.base_dir, actor, Some(timeout))
        .await
        .map_err(|e| e.to_string())
}

/// 装配 JS actor：逐 db.<name> 开库（共享单池，对齐 Go 共享 *sql.DB）→ 仅 default 播种 →
/// Bridge::with_dbs 构造期注入 → pool(PoolSize) 个 actor 线程。
async fn build_actor(cfg: &Config) -> Result<JsActor, String> {
    for name in cfg.redis.keys() {
        eprintln!("warn: redis {name:?} configured but ignored (M0 内存 KV)");
    }
    let mut dbs: HashMap<String, Arc<dyn DataAccessor>> = HashMap::new();
    for (name, dc) in &cfg.db {
        let acc = SqlxAccessor::arc(&dc.dsn)
            .await
            .map_err(|e| format!("open sqlite {name:?}: {e}"))?;
        if name == "default" {
            seed_demo(acc.as_ref()).await?;
        }
        dbs.insert(name.clone(), acc);
    }
    let n = cfg.server.pool_size.max(1) as usize;
    Ok(JsActor::pool(n, move || {
        Bridge::with_dbs(
            dbs.clone(),
            Arc::new(InMemoryKV::new()),
            demo_registry(),
            false,
        )
    }))
}

/// 演示 schema 白名单（db.table 构造器可用 user_profile）。
fn demo_registry() -> SchemaRegistry {
    SchemaRegistry::new().table("user_profile", Some("id"), &["id", "name", "role"])
}

/// ":8080" → "0.0.0.0:8080"（Go fiber 接受裸端口，SocketAddr 不接受）。
fn normalize_addr(a: &str) -> Result<std::net::SocketAddr, String> {
    let s = if a.starts_with(':') { format!("0.0.0.0{a}") } else { a.to_string() };
    s.parse().map_err(|e| format!("invalid addr {a:?}: {e}"))
}

/// user_profile 演示表 + 种子数据（Go SeedDemoDB 等价，可重复运行）。
async fn seed_demo(db: &dyn DataAccessor) -> Result<(), String> {
    let wrap = |e: Box<dyn std::error::Error + Send + Sync>| format!("seed demo db: {e}");
    db.exec_with_params(
        "CREATE TABLE IF NOT EXISTS user_profile (id INTEGER PRIMARY KEY, name TEXT NOT NULL, role TEXT NOT NULL)",
        &[],
    )
    .await
    .map_err(wrap)?;
    db.exec_with_params("DELETE FROM user_profile", &[]).await.map_err(wrap)?;
    for (id, name, role) in [(1, "neo", "admin"), (2, "trinity", "user"), (3, "morpheus", "admin")] {
        db.exec_with_params(
            "INSERT INTO user_profile (id, name, role) VALUES (?, ?, ?)",
            &[serde_json::json!(id), serde_json::json!(name), serde_json::json!(role)],
        )
        .await
        .map_err(wrap)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_args_branches() {
        let cases: &[(&[&str], &str, &str, bool)] = &[
            (&[], "", "", false),
            (&["--config", "x.yml"], "x.yml", "", false),
            (&["--env", "prod"], "", "prod", false),
            (&["--generate-config"], "", "", true),
            (&["--config", "c.yml", "--env", "staging"], "c.yml", "staging", false),
        ];
        for (a, cfg, env, is_gen) in cases {
            let (c, e, g) = parse_args(&args(a));
            assert_eq!(
                (c.as_str(), e.as_str(), g),
                (*cfg, *env, *is_gen),
                "args: {a:?}"
            );
        }
    }

    struct TempDir(std::path::PathBuf);
    fn dir() -> TempDir {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "mdm-dev-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&base).unwrap();
        TempDir(base)
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn generate_config_writes_file() {
        let t = dir();
        let out = t.0.join("out.yml");
        run_from(
            &t.0,
            &args(&["--generate-config", "--config", out.to_str().unwrap()]),
            false,
        )
        .await
        .unwrap();
        assert!(out.is_file());
    }

    #[tokio::test]
    async fn default_wiring_no_listen() {
        // 零配置启动：默认 sqlite::memory: 开库 + 播种 + actor 池，装配成功即可。
        let t = dir();
        run_from(&t.0, &[], false).await.unwrap();
    }

    #[tokio::test]
    async fn missing_explicit_config_errors() {
        let t = dir();
        let err = run_from(&t.0, &args(&["--config", "/no/such/cfg.yml"]), false)
            .await
            .err()
            .unwrap_or_default();
        assert!(err.contains("not found"), "{err}");
    }

    #[tokio::test]
    async fn bad_dsn_errors() {
        // DSN 指向不存在路径 → 开库失败上抛（Go TestBuildServer_SeedError 等价）。
        let t = dir();
        let cfg = t.0.join("cfg.yml");
        std::fs::write(
            &cfg,
            format!("db:\n  default:\n    dsn: 'sqlite://{}/no_dir/x.db'\n", t.0.display()),
        )
        .unwrap();
        let err = run_from(&t.0, &args(&["--config", "cfg.yml"]), false)
            .await
            .err()
            .unwrap_or_default();
        assert!(err.contains("open sqlite"), "{err}");
    }
}
