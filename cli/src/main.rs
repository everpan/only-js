mod args;
mod server_cmd; // T11 填充；先放占位模块见 Step 6

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match args::parse(&argv) {
        args::Command::None => {
            eprintln!("usage: oj <server|build> [flags]\n  oj server -c config.yaml -b /v1/api -d src --dev");
            std::process::exit(2);
        }
        args::Command::Build(_) => {
            eprintln!("oj build: not implemented (v0.1)");
            std::process::exit(2);
        }
        args::Command::Server(a) => {
            if let Err(e) = server_cmd::run(a) {
                eprintln!("oj server: {e}");
                std::process::exit(1);
            }
        }
    }
}
