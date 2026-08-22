//! oj 参数解析（纯函数）。v0.1 子命令：server（build 占位）。

/// server 子命令参数。
pub struct ServerArgs {
    pub config: String,
    pub base: String,
    pub dir: String,
    pub dev: bool,
}

/// 解析结果。None = 无子命令（main 打用法）。
pub enum Command {
    Server(ServerArgs),
    Build(Vec<String>),
    None,
}

/// `oj server [-c config.yaml] [-b /v1/api] [-d src|dist] [--dev]`。
/// -d 缺省：--dev → src，否则 dist。
pub fn parse(args: &[String]) -> Command {
    let mut it = args.iter();
    match it.next().map(|s| s.as_str()) {
        Some("build") => Command::Build(args[1..].to_vec()),
        Some("server") => {
            let (mut config, mut base, mut dir, mut dev) =
                (String::new(), String::new(), String::new(), false);
            let mut cur = it.clone().peekable();
            while let Some(a) = cur.next() {
                match a.as_str() {
                    "-c" | "-b" | "-d" => {
                        if let Some(v) = cur.next() {
                            match a.as_str() {
                                "-c" => config = v.clone(),
                                "-b" => base = v.clone(),
                                _ => dir = v.clone(),
                            }
                        }
                    }
                    "--dev" => dev = true,
                    _ => {}
                }
            }
            // 无任何标志时，默认 dev=true
            if config.is_empty() && base.is_empty() && dir.is_empty() && !dev {
                dev = true;
            }
            let dir = if dir.is_empty() { if dev { "src".into() } else { "dist".into() } } else { dir };
            Command::Server(ServerArgs {
                config: if config.is_empty() { "config.yaml".into() } else { config },
                base: if base.is_empty() { "/v1/api".into() } else { base },
                dir,
                dev,
            })
        }
        _ => Command::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_branches() {
        // 无参 → None；server 默认值；显式覆盖；build 占位。
        assert!(matches!(parse(&args(&[])), Command::None));
        let Command::Server(a) = parse(&args(&["server"])) else { panic!() };
        assert_eq!((a.config.as_str(), a.base.as_str(), a.dir.as_str(), a.dev),
                   ("config.yaml", "/v1/api", "src", true));
        let Command::Server(a) = parse(&args(&["server", "-c", "c.yaml", "-b", "/api",
                                               "-d", "dist"])) else { panic!() };
        assert_eq!((a.config.as_str(), a.base.as_str(), a.dir.as_str(), a.dev),
                   ("c.yaml", "/api", "dist", false));
        let Command::Server(a) = parse(&args(&["server", "--dev", "-d", "x"])) else { panic!() };
        assert!(a.dev && a.dir == "x");
        assert!(matches!(parse(&args(&["build", "moduleA"])), Command::Build(_)));
    }
}
