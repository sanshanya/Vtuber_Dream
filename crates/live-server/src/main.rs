//! live-audience CLI（D-6：M4 单入口 `demo`；serve/run 挂 M5）。
//!
//! 参数解析与命令分发 only（AGENTS.md §5 cli.py 边界）；手写最小解析：
//! 三个参数形态（demo / -c|--config / --output），不值得引入 clap。

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "用法: live-audience demo [-c|--config <config.yaml>] [--output <目录>]";

enum Parse {
    Demo {
        config: PathBuf,
        output: Option<PathBuf>,
    },
    Help,
    Usage(String),
}

// r6 F-4：argparse 吞不下「长得就像已注册选项」的值（`--output -c` →
// expected one argument + exit 2）。`-` 前缀一律拒：孤 `-` 与生僻负目录没有
// 真实使用者，规则越简越 parity。
fn looks_like_option(value: &str) -> bool {
    value.starts_with('-')
}

fn parse(args: &[String]) -> Parse {
    let mut rest = args.iter();
    // r6 F-1：Python argparse `subparsers required=True` → 裸调用 usage + exit 2。
    match rest.next().map(String::as_str) {
        None => return Parse::Usage("缺命令".to_string()),
        Some("-h") | Some("--help") => return Parse::Help,
        Some("demo") => {}
        Some(other) => return Parse::Usage(format!("未知命令 {other}")),
    }
    let mut config = PathBuf::from("config.yaml");
    let mut output = None;
    while let Some(arg) = rest.next() {
        // r6 F-2：argparse 子命令帮助是 exit 0，不是用法错误。
        match arg.as_str() {
            "-h" | "--help" => return Parse::Help,
            "-c" | "--config" => match rest.next() {
                Some(value) if looks_like_option(value) => {
                    return Parse::Usage(format!("{arg} 缺路径（{value} 是选项）"));
                }
                Some(value) => config = PathBuf::from(value),
                None => return Parse::Usage(format!("{arg} 缺路径")),
            },
            "--output" => match rest.next() {
                Some(value) if looks_like_option(value) => {
                    return Parse::Usage(format!("{arg} 缺目录（{value} 是选项）"));
                }
                Some(value) => output = Some(PathBuf::from(value)),
                None => return Parse::Usage("--output 缺目录".to_string()),
            },
            other => return Parse::Usage(format!("未知参数 {other}")),
        }
    }
    Parse::Demo { config, output }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse(&args) {
        Parse::Help => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Parse::Usage(reason) => {
            eprintln!("{reason}\n{USAGE}");
            ExitCode::from(2)
        }
        Parse::Demo { config, output } => {
            let run = live_core::config::load_config(&config)
                .map_err(|error| error.to_string())
                .and_then(|cfg| {
                    live_core::demo::build_demo(&cfg, output.as_deref())
                        .map_err(|error| error.to_string())
                });
            match run {
                // Python cli.py demo：json.dumps(..., indent=2, ensure_ascii=False)。
                Ok(result) => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&result).expect("demo 返回可序列化")
                    );
                    ExitCode::SUCCESS
                }
                // r6 F-3：Python cli.py 广谱 except → stderr `error: {exc}` + return 2。
                Err(error) => {
                    eprintln!("error: {error}");
                    ExitCode::from(2)
                }
            }
        }
    }
}
