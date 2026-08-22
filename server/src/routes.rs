//! 目录镜像路由：URL = base 之后的目录路径 → `<root>/<path>/api.(ts|js)`。
//! 任意深度；无 api 文件的目录不是路由（可作纯工具代码目录）。

use std::path::{Path, PathBuf};

/// 目录镜像路由器。ts=true（--dev）找 api.ts，否则 api.js。
#[derive(Clone)]
pub struct Routes {
    base: String,
    root: PathBuf,
    ts: bool,
}

impl Routes {
    pub fn new(base: &str, root: impl Into<PathBuf>, ts: bool) -> Self {
        // 归一 base：保证前后各一个 '/'（"/v1/api" 与 "/v1/api/" 等价）。
        let base = format!("/{}/", base.trim_matches('/'));
        Self { base, root: root.into(), ts }
    }

    /// 解析 HTTP 路径 → api 文件绝对路径；目录不存在/越界/非文件 → None。
    pub fn resolve(&self, http_path: &str) -> Option<PathBuf> {
        let rel = http_path.strip_prefix(self.base.as_str())?;
        let rel = rel.trim_matches('/');
        if rel.is_empty() {
            return None;
        }
        // 安全：拒绝空段与越界段（目录穿越按 404 处理）。
        if rel.split('/').any(|s| {
            s.is_empty() || s == ".." || s == "." || s.contains('\\') || s.contains('\0')
        }) {
            return None;
        }
        let file = self.root.join(rel).join(if self.ts { "api.ts" } else { "api.js" });
        file.is_file().then_some(file)
    }
}

/// HTTP 动词 → handler 方法名（全表；DELETE→del）。未映射 → None（405）。
pub fn method_name(m: &str) -> Option<&'static str> {
    match m {
        "GET" => Some("get"),
        "POST" => Some("post"),
        "PUT" => Some("put"),
        "DELETE" => Some("del"),
        "PATCH" => Some("patch"),
        "HEAD" => Some("head"),
        "OPTIONS" => Some("options"),
        _ => None,
    }
}

/// 收集 root 下全部路由相对路径（启动时打印路由表用，UC-8）。
pub fn route_table(root: &Path, ts: bool) -> Vec<String> {
    let ext = if ts { "api.ts" } else { "api.js" };
    let mut out = Vec::new();
    walk(root, ext, &mut Vec::new(), &mut out);
    out.sort();
    out
}

fn walk(dir: &Path, ext: &str, rel: &mut Vec<String>, acc: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            rel.push(name);
            walk(&p, ext, rel, acc);
            rel.pop();
        } else if name == ext {
            acc.push(rel.join("/"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(files: &[&str]) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!(
            "oj-routes-{}-{:p}", std::process::id(), files as *const _
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        for rel in files {
            let p = base.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "// api").unwrap();
        }
        base
    }

    #[test]
    fn mirrors_directory_tree_any_depth() {
        let root = fixture(&["user/account/api.ts", "user/profile/detail/api.ts"]);
        let r = Routes::new("/v1/api", &root, true);
        assert_eq!(r.resolve("/v1/api/user/account/"), Some(root.join("user/account/api.ts")));
        assert_eq!(r.resolve("/v1/api/user/account"), Some(root.join("user/account/api.ts")));
        assert_eq!(r.resolve("/v1/api/user/profile/detail/"),
                   Some(root.join("user/profile/detail/api.ts")));
    }

    #[test]
    fn missing_or_traversal_is_none() {
        let root = fixture(&["user/account/api.ts"]);
        let r = Routes::new("/v1/api", &root, true);
        assert_eq!(r.resolve("/v1/api/none/here/"), None);
        assert_eq!(r.resolve("/v1/api/../etc/"), None);
        assert_eq!(r.resolve("/v1/api//dbl/"), None);
        assert_eq!(r.resolve("/other/base/user/account/"), None);
        assert_eq!(r.resolve("/v1/api/"), None);
        assert_eq!(r.resolve("/v1/apifoo/user/"), None);
    }

    #[test]
    fn release_mode_maps_api_js() {
        let root = fixture(&["user/account/api.js"]);
        assert!(Routes::new("/v1/api", &root, false).resolve("/v1/api/user/account/").is_some());
        assert!(Routes::new("/v1/api", &root, true).resolve("/v1/api/user/account/").is_none());
    }

    #[test]
    fn method_table_complete() {
        assert_eq!(method_name("GET"), Some("get"));
        assert_eq!(method_name("DELETE"), Some("del"));
        assert_eq!(method_name("PATCH"), Some("patch"));
        assert_eq!(method_name("HEAD"), Some("head"));
        assert_eq!(method_name("OPTIONS"), Some("options"));
        assert_eq!(method_name("TRACE"), None);
    }
}
