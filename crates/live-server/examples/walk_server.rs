//! M5-D 面板走查用的静态 serve：以「导出 delta 布景」为 data_root 起真端口。
//!
//! 用法：
//! ```shell
//! cargo run -p live-server --example walk_server -- <config.yaml> <demo_root> [port]
//! ```
//! 本例属走查工序——只绑 127.0.0.1，Ctrl-C 退出。

use std::path::PathBuf;

use live_server::app::{StartOptions, serve};

fn main() {
    let mut args = std::env::args().skip(1);
    let config_path = PathBuf::from(args.next().expect("config.yaml 路径"));
    let data_root = PathBuf::from(args.next().expect("demo_root 路径"));
    let port: u16 = args
        .next()
        .map(|v| v.parse().expect("port"))
        .unwrap_or(3795);
    if let Err(error) = serve(StartOptions {
        config_path,
        port,
        web_root: PathBuf::from("web/dist"),
        demo: false,
        data_root: Some(data_root),
        bilibili_hosts: None,
    }) {
        eprintln!("error: {error}");
        std::process::exit(2);
    }
}
