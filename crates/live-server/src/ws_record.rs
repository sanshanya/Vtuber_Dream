//! WS 弹幕窗独立录制入口（删码刀9：从 Agent run 槽解耦）。
//!
//! 解法：事实面采录（最长 12h 保险丝）与认知 run（单 run 互斥槽）是两个
//! 平面——合一会让一场直播整夜堵住全部 Agent run。现拆为显式入口：
//! `live-audience ws-record -c config.yaml`（或库件 run_ws_record 直呼）——
//! 不占 run registry、不占用例面 kind；输出窗摘要（同原 outcome.ws_window 键形）。
//! 协议子系统（codec/auth/心跳/重连/PREPARING 语义）原样保留在 live-core::live_ws。

use serde_json::{Value, json};

use live_core::config::Config;
use live_core::episodes::{now_iso, now_unix_secs};
use live_core::live_ws::episodes::{WsRecorder, ingest_ws_window};
use live_core::live_ws::replay::replay_jsonl;
use live_core::live_ws::session::{WsSessionConfig, run_session};

/// WS 弹幕窗采录全过程（同步阻塞直至收窗）：
/// 1. 房间在播（`get_room_live_status==1`）才开窗；
/// 2. `get_danmu_info` 拿网关 host/port/token → `WsSessionConfig`（cookie 只进握手头）；
/// 3. current_thread runtime 跑 `run_session`，事件流直灌 `WsRecorder`；
/// 4. `finish` 收窗 → 窗线入账 graph（kind=ws-record；对照窗排除见 query.rs）→ 摘要。
///
/// 返回 `Ok(None)` = 房间未在播（未开窗）/开窗校验未过；
/// `Ok(Some(Value))` = 窗摘要（lines / counts / end_reason / session / unknowns）；
/// `Err` = 开窗后失败（客户端构造/在播探测/凭据/会话 runtime 任一步崩）。
pub fn run_ws_record(
    config: &Config,
    bilibili_hosts: Option<(String, String)>,
    emit: &dyn Fn(&str),
) -> Result<Option<Value>, String> {
    let Ok(room_id) = config.bilibili.room_id.parse::<i64>() else {
        return Err(format!(
            "房间号非法（{}），无法采录",
            config.bilibili.room_id
        ));
    };
    emit(&format!("[WS] 探测房间 {room_id} 在播状态…"));
    let client = match bilibili_hosts {
        Some((api, live)) => live_core::bilibili::BilibiliClient::with_origin(
            &api,
            &live,
            &config.bilibili.cookie,
            config.collection.request_delay_seconds,
            config.collection.timeout_seconds,
        ),
        None => live_core::bilibili::BilibiliClient::new(
            &config.bilibili.cookie,
            config.collection.request_delay_seconds,
            config.collection.timeout_seconds,
        ),
    }
    .map_err(|error| error.to_string())?;
    let mut client = client;

    let live_status = client
        .get_room_live_status(&config.bilibili.room_id)
        .map_err(|error| error.to_string())?;
    if live_status != 1 {
        emit(&format!(
            "[WS] 房间未在播（live_status={live_status}），本窗不开"
        ));
        return Ok(None);
    }

    emit("[WS] 房间在播，建立弹幕网关凭据…");
    let danmu = client
        .get_danmu_info(&config.bilibili.room_id)
        .map_err(|error| error.to_string())?;
    let mut session_cfg = WsSessionConfig::new(danmu.url(), room_id, danmu.token);
    // cookie 只进 WS 握手头（§11 红线：绝不进任何错误串/日志面）。
    session_cfg.cookie = config.bilibili.cookie.clone();

    let Some(mut recorder) = WsRecorder::attach(room_id, now_unix_secs(), 1) else {
        emit("[WS] 开窗失败（在播校验未过），本窗不开");
        return Ok(None);
    };
    emit(&format!("[WS] 弹幕窗开启（rid={room_id}，起点 attach）…"));

    // 会话尽量跑：PREPARING 关窗 / 断连重连预算尽 / 12h 保险丝 / 认证失败。
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("tokio runtime: {error}"))?;
    let report = rt.block_on(run_session(
        &session_cfg,
        &mut |ev| {
            recorder.on_event(ev, now_unix_secs());
            Ok(())
        },
        &now_unix_secs,
    ))?;

    let capture = recorder.finish(&report.end, now_unix_secs());
    emit(&format!(
        "[WS] 弹幕窗收窗：end_reason={}，线 {} 条，未知 {} 行",
        capture.end_reason,
        capture.episodes.len(),
        capture.unknowns.len()
    ));

    // 窗线入账 graph（kind=ws-record，pipeline 数据面之外的账本容器——对照窗
    // 排除，语义注释见 graph/query.rs run_pair_delta）。
    match live_core::graph::Store::open(&config.output_dir.join("graph").join("perception.sqlite3"))
    {
        Ok(store) => {
            let graph_run = format!("run:{}", uuid::Uuid::new_v4().simple());
            if let Err(err) = store.begin_run_typed(
                &graph_run,
                &now_iso(),
                live_core::graph::Store::RUN_KIND_WS_RECORD,
                live_core::graph::Store::RUN_KIND_WS_RECORD,
                None,
            ) {
                emit(&format!("[WS] 弹幕窗 run 开账失败：{err}"));
            } else {
                match ingest_ws_window(&store, &graph_run, &capture) {
                    Ok(()) => match store.complete_run(&graph_run) {
                        Ok(()) => emit(&format!(
                            "[WS] 弹幕窗线入账完成（{} 线）",
                            capture.episodes.len()
                        )),
                        Err(err) => emit(&format!("[WS] 弹幕窗 run 结清失败：{err}")),
                    },
                    Err(err) => {
                        emit(&format!("[WS] 弹幕窗线入账失败：{err}"));
                        if let Err(close_err) = store.fail_run(&graph_run, &err.to_string(), false)
                        {
                            emit(&format!("[WS] 弹幕窗 run 记败失败：{close_err}"));
                        }
                    }
                }
            }
        }
        Err(err) => emit(&format!(
            "[WS] graph store 打开失败（弹幕窗线未入账）：{err}"
        )),
    }

    let ws_window = json!({
        "lines": capture.episodes.len(),
        "session": capture.session,
        "unknowns": capture.unknowns,
        "counts": capture.counts,
        "end_reason": capture.end_reason,
    });
    Ok(Some(ws_window))
}

/// 外部录播 JSONL 回放入图（离线闭轨，不占 run 槽）：
/// 多文件合并 → `WsRecorder` 同轨窗 → kind=ws-record 入账（事实现平面，对照
/// 窗排除同实时轨）→ 复盘卡即刷（回放是闭轨命令，刷复盘是收尾本体）。
///
/// 返回摘要 JSON：ws_window（同实时轨键形）+ rows/skipped/families/money/recap。
/// Err = 无可入账行 / 落账失败 / 复盘刷新失败（失败显形不装成功）。
pub fn run_ws_replay(
    config: &Config,
    paths: &[std::path::PathBuf],
    emit: &dyn Fn(&str),
) -> Result<Value, String> {
    let Ok(room_id) = config.bilibili.room_id.parse::<i64>() else {
        return Err(format!(
            "房间号非法（{}），无法回放",
            config.bilibili.room_id
        ));
    };
    emit(&format!("[回放] 读入 {} 份录播底稿…", paths.len()));
    let outcome = replay_jsonl(room_id, paths)?;
    let total_lines: usize = outcome.captures.iter().map(|c| c.episodes.len()).sum();
    emit(&format!(
        "[回放] 行账 {}/{}（跳 {}），分 {} 场，共 {} 线，礼物 ¥{:.1} + SC ¥{:.1}，上舰 {} 播报 {}",
        outcome.rows - outcome.skipped,
        outcome.rows,
        outcome.skipped,
        outcome.captures.len(),
        total_lines,
        outcome.money.gift_yuan,
        outcome.money.sc_yuan,
        outcome.money.guard_count,
        outcome.money.toast_count,
    ));

    let store =
        live_core::graph::Store::open(&config.output_dir.join("graph").join("perception.sqlite3"))
            .map_err(|err| format!("graph store 打开失败：{err}"))?;
    let graph_run = format!("run:{}", uuid::Uuid::new_v4().simple());
    store
        .begin_run_typed(
            &graph_run,
            &now_iso(),
            live_core::graph::Store::RUN_KIND_WS_RECORD,
            live_core::graph::Store::RUN_KIND_WS_RECORD,
            None,
        )
        .map_err(|err| format!("回放 run 开账失败：{err}"))?;
    // 逐窗入账（一场一 run 无关，幂等键撞库归 stable——同窗重放无重复行）。
    for capture in &outcome.captures {
        if let Err(err) = ingest_ws_window(&store, &graph_run, capture) {
            let _ = store.fail_run(&graph_run, &err.to_string(), false);
            return Err(format!("回放线入账失败：{err}"));
        }
    }
    store
        .complete_run(&graph_run)
        .map_err(|err| format!("回放 run 结清失败：{err}"))?;
    emit(&format!(
        "[回放] 入账完成（run={graph_run}，{} 场，{} 线）",
        outcome.captures.len(),
        total_lines
    ));

    emit("[回放] 刷新复盘卡…");
    let recap = live_core::recap::refresh_recap_card(&config.output_dir, emit)
        .map_err(|err| format!("复盘卡刷新失败：{err}"))?;

    let window_json = |capture: &live_core::live_ws::episodes::WsWindowCapture| {
        json!({
            "lines": capture.episodes.len(),
            "session": capture.session,
            "unknowns": capture.unknowns,
            "counts": capture.counts,
            "end_reason": capture.end_reason,
        })
    };
    let windows: Vec<Value> = outcome.captures.iter().map(window_json).collect();
    Ok(json!({
        // 兼容键（哨兵当夜报告同款老消费面）：末窗 = 最新场。
        "ws_window": windows.last().cloned().unwrap_or(Value::Null),
        "ws_windows": windows,
        "rows": outcome.rows,
        "skipped": outcome.skipped,
        "families": outcome.families,
        "money": outcome.money,
        "recap": recap,
    }))
}
