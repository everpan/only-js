//! devserver 可执行入口（薄壳；逻辑在 mdm_server::devserver，便于测试）。

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = mdm_server::devserver::run_with(&args, true).await {
        eprintln!("devserver: {e}");
        std::process::exit(1);
    }
}
