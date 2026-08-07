//! live-audience CLI。
//!
//! 参数解析与命令分发 only（AGENTS.md 目标模块边界）；手写最小解析——
//! 三个子命令（agent-check/serve/graph-reconcile），选项集不同，不值得引入 clap。

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "用法: live-audience <命令>\n  agent-check [-c|--config <config.yaml>]              真实端点探针验收（opt-in：需环境变量 VTD_AGENT_CHECK=1）\n  serve [-c|--config <config.yaml>] [--port <n>]         本地服务（默认 3781）\n  graph-reconcile [-c|--config <config.yaml>]            长期实体「AI 归并」单轮回放\n  ws-record [-c|--config <config.yaml>]                  WS 弹幕窗独立采录（不占 run 槽；止：PREPARING/12h 保险丝）";

/// 真实端点验收的显式开关值（AGENTS.md 质量门禁：真实端点必须 opt-in）。只认 "1"。
pub const AGENT_CHECK_ENV: &str = "VTD_AGENT_CHECK";

enum Parse {
    AgentCheck { config: PathBuf },
    Serve { config: PathBuf, port: u16 },
    GraphReconcile { config: PathBuf },
    WsRecord { config: PathBuf },
    Help,
    Usage(String),
}

// r6 F-4：argparse 吞不下「长得就像已注册选项」的值（`--output -c` →
// expected one argument + exit 2）。`-` 前缀一律拒：孤 `-` 与生僻负目录没有
// 真实使用者，规则越简越 parity。
fn looks_like_option(value: &str) -> bool {
    value.starts_with('-')
}

/// `[-c|--config <path>]` 消费函数（四种臂共用）。
fn take_config(
    arg: &str,
    rest: &mut std::slice::Iter<String>,
    config: &mut PathBuf,
) -> Result<bool, String> {
    match arg {
        "-c" | "--config" => match rest.next() {
            Some(value) if looks_like_option(value) => {
                Err(format!("{arg} 缺路径（{value} 是选项）"))
            }
            Some(value) => {
                *config = PathBuf::from(value);
                Ok(true)
            }
            None => Err(format!("{arg} 缺路径")),
        },
        _ => Ok(false),
    }
}

fn parse_port(rest: &mut std::slice::Iter<String>, arg: &str) -> Result<u16, String> {
    match rest.next() {
        Some(value) if looks_like_option(value) => Err(format!("{arg} 缺端口（{value} 是选项）")),
        Some(value) => value
            .parse::<u16>()
            .map_err(|_| format!("{arg} 端口必须是 0-65535 的整数（收到 {value}）")),
        None => Err(format!("{arg} 缺端口")),
    }
}

fn parse(args: &[String]) -> Parse {
    let mut rest = args.iter();
    // r6 F-1：Python argparse `subparsers required=True` → 裸调用 usage + exit 2。
    let Some(command) = rest.next().map(String::as_str) else {
        return Parse::Usage("缺命令".to_string());
    };
    if command == "-h" || command == "--help" {
        return Parse::Help;
    }
    if command != "agent-check"
        && command != "serve"
        && command != "graph-reconcile"
        && command != "ws-record"
    {
        return Parse::Usage(format!("未知命令 {command}"));
    }
    let mut config = PathBuf::from("config.yaml");
    let mut port = live_server::app::DEFAULT_PORT;
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "-h" | "--help" => return Parse::Help,
            "-c" | "--config" => {
                if let Err(reason) = take_config(arg.as_str(), &mut rest, &mut config) {
                    return Parse::Usage(reason);
                }
            }
            "--port" => match parse_port(&mut rest, "--port") {
                Ok(value) => port = value,
                Err(reason) => return Parse::Usage(reason),
            },
            other => return Parse::Usage(format!("未知参数 {other}")),
        }
    }
    match command {
        "agent-check" => Parse::AgentCheck { config },
        "serve" => Parse::Serve { config, port },
        "ws-record" => Parse::WsRecord { config },
        _ => Parse::GraphReconcile { config },
    }
}

/// demo/agent-check 共用车道：Ok → pretty JSON stdout；Err → `error: {e}` + exit 2
/// （r6 F-3：Python cli.py 广谱 except 形态）。
fn run_json_task(task: impl FnOnce() -> Result<serde_json::Value, String>) -> ExitCode {
    match task() {
        Ok(result) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&result).expect("任务返回可序列化")
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(2)
        }
    }
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
        Parse::AgentCheck { config } => {
            // env 门先行：未 opt-in 时连 config 都不读（AGENTS.md 质量门禁·真实端点条款；
            // 钉：agent_check_cli.rs 拒门用例给了不存在的配置路径仍报门）。
            if std::env::var(AGENT_CHECK_ENV).ok().as_deref() != Some("1") {
                eprintln!(
                    "agent-check 是真实端点验收，需显式 opt-in：请先设置 {AGENT_CHECK_ENV}=1 再运行\n{USAGE}"
                );
                return ExitCode::from(2);
            }
            run_json_task(|| {
                live_core::config::load_config(&config)
                    .map_err(|error| error.to_string())
                    .and_then(|cfg| {
                        live_core::agent::probe::run_agent_check(&cfg)
                            .map_err(|error| error.to_string())
                    })
            })
        }
        Parse::Serve { config, port } => serve_command(config, port),
        Parse::WsRecord { config } => run_json_task(|| {
            live_core::config::load_config(&config)
                .map_err(|error| error.to_string())
                .and_then(|cfg| {
                    live_server::ws_record::run_ws_record(&cfg, None, &|message| {
                        eprintln!("{message}")
                    })
                })?
                .map(|window| serde_json::to_value(window).map_err(|error| error.to_string()))
                .unwrap_or_else(|| {
                    Ok(serde_json::json!({"status": "skipped", "detail": "房间未在播或未开窗"}))
                })
        }),
        Parse::GraphReconcile { config } => run_json_task(|| {
            live_core::config::load_config(&config)
                .map_err(|error| error.to_string())
                .and_then(|cfg| {
                    let runtime = tokio::runtime::Builder::new_multi_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| format!("tokio runtime: {error}"))?;
                    let graph_file = cfg.output_dir.join("graph").join("perception.sqlite3");
                    let store = live_core::graph::store::Store::open(&graph_file)
                        .map_err(|error| format!("graph store: {error}"))?;
                    let report = runtime
                        .block_on(live_core::agent::reconcile::run_entity_reconcile(
                            &live_core::agent::runtime::AgentRuntime::from_ai_config(&cfg.ai)
                                .map_err(|error| error.to_string())?,
                            &cfg,
                            &store,
                        ))
                        .map_err(|error| error.to_string())?;
                    serde_json::to_value(report).map_err(|error| error.to_string())
                })
        }),
    }
}

/// serve 启动面（B1）：端口。
fn serve_command(config: PathBuf, port: u16) -> ExitCode {
    match live_server::app::serve(live_server::app::StartOptions {
        config_path: config,
        port,
        web_root: PathBuf::from("web/dist"),
        // 生产恒走官方端点；测试面由 AppState 同名 seam 注入 wiremock 根。
        bilibili_hosts: None,
    }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(2)
        }
    }
}
