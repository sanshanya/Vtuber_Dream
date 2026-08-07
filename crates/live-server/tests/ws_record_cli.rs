//! `ws-record` 子命令接缝钉：BINARY 面接线完整性。
//!
//! 1. 配置路径不存在 → `error: {load}` + exit 2（与普通命令同 exit1 面）。
//! 2. 未知选项 → exit 2 + usage 指引。
//!
//! 采录全链与房间在播/未在播/认证拒绝/幂等语义由 tests/app_runs_e2e_ws.rs 承保。

use std::process::Command;

#[test]
fn ws_record_missing_config_errors_exit_2() {
    let output = Command::new(env!("CARGO_BIN_EXE_live-audience"))
        .args(["ws-record", "-c", "不存在的-config.yaml"])
        .output()
        .expect("spawn live-audience");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("error: "), "{stderr}");
}

#[test]
fn ws_record_unknown_option_is_usage_2() {
    let output = Command::new(env!("CARGO_BIN_EXE_live-audience"))
        .args(["ws-record", "--bogus"])
        .output()
        .expect("spawn live-audience");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("未知参数") && stderr.contains("用法: "),
        "{stderr}"
    );
}
