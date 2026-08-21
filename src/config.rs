//! 配置层（移植 Go internal/config）。
//!
//! 叠加语义（对齐 Go yaml.Unmarshal 到已填充 struct）：`Default()` ← `cfg.yml` ← `cfg.<env>.yml`，
//! map 按键合并、标量/序列整体覆盖、未提字段保留；env 来自 `--env`/`APP_ENV`（**文件叠加，非 OS 环境变量**）。
//! Duration 以字符串存储（"5s"/"500ms"），使用方解析；Go 的裸数字纳秒形式不支持（ponytail: 需要时再补）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_yaml::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub server: ServerConfig,
    /// 命名 DB 实例，键即 JS `DB(name)` 的 conf-field-name；"default" 为默认。
    pub db: HashMap<String, DBConfig>,
    /// 命名 Redis 实例。
    pub redis: HashMap<String, RedisConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerConfig {
    pub addr: String,
    pub base_dir: String,
    /// 单次请求执行超时（如 "5s"）。
    pub timeout: String,
    pub pool_size: i32,
    pub hmr: HMRConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HMRConfig {
    pub enabled: bool,
    /// 热重载根目录，空则复用 base_dir。
    pub root: String,
    /// 轮询间隔（如 "500ms"）。
    pub interval: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DBConfig {
    pub dsn: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RedisConfig {
    pub addr: String,
    pub password: String,
    pub db: i32,
}

impl Default for Config {
    /// 内置默认值（等价 Go Default()）。
    fn default() -> Self {
        Self {
            server: ServerConfig {
                addr: ":8080".into(),
                base_dir: "routes".into(),
                timeout: "5s".into(),
                pool_size: 8,
                hmr: HMRConfig {
                    enabled: true,
                    root: String::new(),
                    interval: "500ms".into(),
                },
            },
            db: HashMap::from([(
                "default".to_string(),
                DBConfig {
                    // Go 用 modernc 方言 "file::memory:?cache=shared"；sqlx 用自身 URL 方言。
                    dsn: "sqlite::memory:".into(),
                },
            )]),
            redis: HashMap::new(),
        }
    }
}

/// 按优先级叠加加载（dir 为查找目录；path 为空尝试 cfg.yml；env 非空叠加 cfg.<env>.yml）。
/// 显式 path 缺失报错；env 文件缺失静默忽略；全部缺失回落默认值。
pub fn load_from(dir: &Path, path: &str, env: &str) -> Result<Config, String> {
    let mut value = serde_yaml::to_value(Config::default()).map_err(|e| e.to_string())?;
    let base = if path.is_empty() { "cfg.yml" } else { path };
    let base_path = dir.join(base);
    if base_path.is_file() {
        value = merge_yaml(value, read_yaml(&base_path)?);
    } else if !path.is_empty() {
        return Err(format!("config {path:?} not found"));
    }
    if !env.is_empty() {
        let env_path = dir.join(format!("cfg.{env}.yml"));
        if env_path.is_file() {
            value = merge_yaml(value, read_yaml(&env_path)?);
        }
    }
    let mut cfg: Config = serde_yaml::from_value(value).map_err(|e| e.to_string())?;
    // 归一化顺序对齐 Go：root 先回落到（此时的）base_dir，再回落 base_dir 本身。
    if cfg.server.hmr.root.is_empty() {
        cfg.server.hmr.root = cfg.server.base_dir.clone();
    }
    if cfg.server.base_dir.is_empty() {
        cfg.server.base_dir = "routes".into();
    }
    Ok(cfg)
}

/// `load_from(Path::new("."), ...)` 的便捷形式。
pub fn load(path: &str, env: &str) -> Result<Config, String> {
    load_from(Path::new("."), path, env)
}

/// 解析 "5s"/"500ms"/"2m" 形式的时长（Go time.ParseDuration 的常用子集；ns/us 不支持）。
pub fn parse_duration(s: &str) -> Result<std::time::Duration, String> {
    let (num, unit) = s.split_at(
        s.find(|c: char| !c.is_ascii_digit() && c != '.')
            .ok_or_else(|| format!("invalid duration {s:?}: no unit"))?,
    );
    let n: f64 = num
        .parse()
        .map_err(|_| format!("invalid duration {s:?}: bad number"))?;
    let millis = match unit {
        "ms" => n,
        "s" => n * 1000.0,
        "m" => n * 60_000.0,
        "h" => n * 3_600_000.0,
        other => return Err(format!("invalid duration unit {other:?} (支持 ms/s/m/h)")),
    };
    Ok(std::time::Duration::from_millis(millis as u64))
}

/// 写出默认配置（--generate-config 起步配置，对齐 Go WriteDefault）。
pub fn write_default(path: &str) -> Result<(), String> {
    let text = serde_yaml::to_string(&Config::default()).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("write default config {path:?}: {e}"))
}

fn read_yaml(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read config {}: {e}", path.display()))?;
    serde_yaml::from_str(&text).map_err(|e| format!("parse config {}: {e}", path.display()))
}

/// 深合并：双侧为 map 时按键递归合并，否则 over 整体覆盖 base。
fn merge_yaml(base: Value, over: Value) -> Value {
    match (base, over) {
        (Value::Mapping(mut b), Value::Mapping(o)) => {
            for (k, v) in o {
                let merged = match b.get(&k) {
                    Some(bv) => merge_yaml(bv.clone(), v),
                    None => v,
                };
                b.insert(k, merged);
            }
            Value::Mapping(b)
        }
        (_, over) => over,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    fn dir(files: &[(&str, &str)]) -> TempDir {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "mdm-cfg-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&base).unwrap();
        for (name, content) in files {
            std::fs::write(base.join(name), content).unwrap();
        }
        TempDir(base)
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn load_defaults_when_no_files() {
        let t = dir(&[]);
        let cfg = load_from(&t.0, "", "").unwrap();
        assert_eq!(cfg.server.addr, ":8080");
        assert_eq!(cfg.server.base_dir, "routes");
        assert_eq!(cfg.server.timeout, "5s");
        assert_eq!(cfg.server.pool_size, 8);
        assert!(cfg.server.hmr.enabled);
        // Load 后 root 已归一化到 base_dir（Default() 是归一化前的值）。
        assert_eq!(cfg.server.hmr.root, "routes");
        assert!(cfg.db.contains_key("default"));
    }

    #[test]
    fn env_overlay_merges_per_key() {
        let t = dir(&[
            ("cfg.yml", "server:\n  addr: ':9000'\ndb:\n  extra:\n    dsn: 'sqlite://x.db'\n"),
            ("cfg.prod.yml", "server:\n  base_dir: routes2\n"),
        ]);
        let cfg = load_from(&t.0, "", "prod").unwrap();
        // 基础文件值保留 + env 覆盖 + db map 按键合并（default 与 extra 并存）。
        assert_eq!(cfg.server.addr, ":9000");
        assert_eq!(cfg.server.base_dir, "routes2");
        assert_eq!(cfg.db.len(), 2);
        assert_eq!(cfg.db["extra"].dsn, "sqlite://x.db");
    }

    #[test]
    fn missing_env_file_silent() {
        let t = dir(&[("cfg.yml", "server:\n  addr: ':9001'\n")]);
        let cfg = load_from(&t.0, "", "staging").unwrap();
        assert_eq!(cfg.server.addr, ":9001");
    }

    #[test]
    fn explicit_missing_config_errors() {
        let t = dir(&[]);
        assert!(load_from(&t.0, "nope.yml", "").is_err());
    }

    #[test]
    fn invalid_yaml_errors() {
        let t = dir(&[("cfg.yml", "server: [broken\n")]);
        assert!(load_from(&t.0, "", "").is_err());
    }

    #[test]
    fn base_dir_empty_falls_back_and_hmr_root_follows() {
        let t = dir(&[("cfg.yml", "server:\n  base_dir: ''\n")]);
        let cfg = load_from(&t.0, "", "").unwrap();
        assert_eq!(cfg.server.base_dir, "routes");
        // 对齐 Go 顺序：root 先于 base_dir 归一化（root 取自当时的 base_dir，此处仍为空）。
        assert_eq!(cfg.server.hmr.root, "");

        let t2 = dir(&[("cfg.yml", "server:\n  base_dir: rt\n")]);
        let cfg2 = load_from(&t2.0, "", "").unwrap();
        assert_eq!(cfg2.server.hmr.root, "rt");
    }

    #[test]
    fn parse_duration_supports_common_units() {
        use std::time::Duration;
        assert_eq!(parse_duration("5s").unwrap(), Duration::from_secs(5));
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
        assert!(parse_duration("5x").is_err());
        assert!(parse_duration("s").is_err());
    }

    #[test]
    fn write_default_roundtrip() {
        let t = dir(&[]);
        let path = t.0.join("gen.yml");
        write_default(path.to_str().unwrap()).unwrap();
        let cfg: Config = serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(cfg, Config::default());
    }
}
