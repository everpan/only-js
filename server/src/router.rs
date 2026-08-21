//! 路由解析（移植 Go internal/router）：`{mode}-{version}/{sub}/{feature}/{entity}/{METHOD}.js`。
//! 首段按**第一个** `-` 切分（对齐 Go `strings.Index` 语义），解析后校验目标文件存在。

use std::path::PathBuf;

/// 解析出的路由参数（对齐 Go Params）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Params {
    pub mode: String,
    pub version: String,
    pub sub: String,
    pub feature: String,
    pub entity: String,
    /// entity 之后的额外段。
    pub rest: Vec<String>,
}

impl Params {
    /// `{mode}-{version}`，routes 根目录名。
    pub fn module(&self) -> String {
        format!("{}-{}", self.mode, self.version)
    }
}

/// 将 (method, path) 解析为 handler 文件与参数（接口便于测试替换）。
pub trait Resolver {
    fn resolve(&self, method: &str, path: &str) -> Option<(PathBuf, Params)>;
}

/// 基于 FS 的解析器，base_dir 一般为 ./routes。
#[derive(Clone)]
pub struct FileResolver {
    base_dir: PathBuf,
}

impl FileResolver {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }
}

impl Resolver for FileResolver {
    fn resolve(&self, method: &str, path: &str) -> Option<(PathBuf, Params)> {
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        if parts.len() < 4 {
            return None;
        }
        // 首段 {mode}-{version}：按第一个 `-` 切分，两侧非空（对齐 Go strings.Index 语义）。
        let (mode, version) = parts[0].split_once('-')?;
        if mode.is_empty() || version.is_empty() {
            return None;
        }
        let p = Params {
            mode: mode.into(),
            version: version.into(),
            sub: parts[1].into(),
            feature: parts[2].into(),
            entity: parts[3].into(),
            rest: parts[4..].iter().map(|s| s.to_string()).collect(),
        };
        let file = self
            .base_dir
            .join(p.module())
            .join(&p.sub)
            .join(&p.feature)
            .join(&p.entity)
            .join(format!("{}.js", method.to_uppercase()));
        if !file.is_file() {
            return None;
        }
        Some((file, p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 测试用 routes 目录（Drop 清理）。
    struct TempRoutes(PathBuf);

    fn routes(files: &[&str]) -> TempRoutes {
        static N: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "mdm-router-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&base).unwrap();
        for rel in files {
            let p = base.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "// handler").unwrap();
        }
        TempRoutes(base)
    }

    impl Drop for TempRoutes {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn resolve_success() {
        let t = routes(&["crm-v1/user/profile/list/GET.js"]);
        let r = FileResolver::new(t.0.clone());
        let (file, p) = r.resolve("GET", "/crm-v1/user/profile/list").unwrap();
        assert_eq!(file, t.0.join("crm-v1/user/profile/list/GET.js"));
        assert_eq!(
            p,
            Params {
                mode: "crm".into(),
                version: "v1".into(),
                sub: "user".into(),
                feature: "profile".into(),
                entity: "list".into(),
                rest: vec![]
            }
        );
    }

    #[test]
    fn resolve_method_file_missing() {
        let t = routes(&["crm-v1/user/profile/list/GET.js"]);
        let r = FileResolver::new(t.0.clone());
        assert!(r.resolve("POST", "/crm-v1/user/profile/list").is_none());
    }

    #[test]
    fn resolve_no_version_segment() {
        let t = routes(&[]);
        let r = FileResolver::new(t.0.clone());
        // 无 -、- 开头、- 结尾均非法（对齐 Go idx<=0 || idx==last）。
        assert!(r.resolve("GET", "/crm/user/profile/list").is_none());
        assert!(r.resolve("GET", "/-v1/user/profile/list").is_none());
        assert!(r.resolve("GET", "/crm-/user/profile/list").is_none());
    }

    #[test]
    fn resolve_too_few_segments() {
        let t = routes(&[]);
        let r = FileResolver::new(t.0.clone());
        assert!(r.resolve("GET", "/crm-v1/user/profile").is_none());
        assert!(r.resolve("GET", "/").is_none());
        assert!(r.resolve("GET", "").is_none());
    }

    #[test]
    fn resolve_extra_rest_captured() {
        let t = routes(&["crm-v1/user/profile/list/GET.js"]);
        let r = FileResolver::new(t.0.clone());
        let (_, p) = r.resolve("GET", "/crm-v1/user/profile/list/a/b").unwrap();
        assert_eq!(p.rest, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn module_helper() {
        let t = routes(&["crm-v1/user/profile/list/GET.js"]);
        let r = FileResolver::new(t.0.clone());
        let (_, p) = r.resolve("GET", "/crm-v1/user/profile/list").unwrap();
        assert_eq!(p.module(), "crm-v1");
    }
}
