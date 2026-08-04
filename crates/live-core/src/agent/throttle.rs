//! Z3/P0-4 AI 电梯·限速漏桶（leaky bucket：终裁 P0-4 第三维——全局 LLM 请求节律门）。
//!
//! 设计（S0 实跑背景：22 观众 × 4 并发 × per-turn 过闸 ⇒ DeepSeek 端零 429；桶是护栏，
//! 不是常驻减速器）：
//! - **漏桶（leaky bucket）**：`rate` req/min 定速放行；`acquire()` 返回的契约 = 恰好一次
//!   LLM 请求出队。per-turn 过闸点（chat 入口）保证「许可 = 请求」1:1。
//! - **预约语义**：等待者在持锁内**预约**自己的放行时刻（next_tick 立即推进），再出锁睡
//!   到该时刻——多等待者的许可时刻天然错开一个间隔，绝无同刻并发放行（over-issue）。
//! - **压缩不失真**：串行场景每张许可恰好推进一个间隔（首许可即时）；并发脉冲 N 张的总
//!   墙钟 = (N-1) 个间隔，无串行化失真（实验门 M≤0.40 的前提）。
//! - **0 = 关闭**：`Throttle::disabled()` 恒无等待；config 默认 0，显式 opt-in。
//! - 时钟一律 tokio::time::Instant：生产 = 真实时钟；测试可 start_paused 虚拟化从而确定性。
//!
//! trace 面：过闸不产生新事件名（events pinned set 钉死零增长）；节流足迹只进墙钟。

use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

/// 漏桶内核：next_tick = 下一张许可的放行时刻。None = 尚未发放过任何许可（首张即时）。
pub struct Throttle {
    inner: Option<Mutex<ThrottleInner>>,
}

struct ThrottleInner {
    next_tick: Option<Instant>,
    interval: Duration,
}

impl Throttle {
    /// 关闭态：acquire 恒立即返回。
    pub fn disabled() -> Self {
        Self { inner: None }
    }

    /// `requests_per_minute <= 0` 等价关闭态（config clamp 的防御副本）。
    pub fn limited(requests_per_minute: i64) -> Self {
        if requests_per_minute <= 0 {
            return Self::disabled();
        }
        Self {
            inner: Some(Mutex::new(ThrottleInner {
                next_tick: None,
                interval: Duration::from_secs_f64(60.0 / requests_per_minute as f64),
            })),
        }
    }

    /// 构造门：rate<=0 ⇒ 关闭态。acquired 许可与 LLM 请求一一对应。
    pub fn build(requests_per_minute: i64) -> Self {
        Self::limited(requests_per_minute)
    }

    /// 取一张许可。返回后调用方须在可预期时间内发出一次 LLM 请求（许可即请求）。
    /// 预约制：决定等待的瞬间即把节拍推进一个间隔——后来者顺排，醒来即走，不再竞争。
    pub async fn acquire(&self) {
        let Some(mutex) = &self.inner else { return };
        let reservation = {
            let mut guard = mutex.lock().expect("throttle poisoned");
            let now = Instant::now();
            match guard.next_tick {
                // 首许可：即时放行，播种节拍。
                None => {
                    guard.next_tick = Some(now + guard.interval);
                    return;
                }
                // 节拍已到（或已过点）：即时放行并推进一个间隔。
                Some(tick) if tick <= now => {
                    guard.next_tick = Some(now + guard.interval);
                    return;
                }
                // 需等待：预约 tick 时刻，节拍先行推进，随后睡到预约点。
                Some(tick) => {
                    guard.next_tick = Some(tick + guard.interval);
                    tick
                }
            }
        };
        tokio::time::sleep_until(reservation).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn disabled_acquire_is_immediate() {
        let throttle = Throttle::disabled();
        for _ in 0..1000 {
            throttle.acquire().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn first_permit_is_immediate_serial_wait_free() {
        let throttle = Throttle::limited(60_000_000);
        for _ in 0..10 {
            throttle.acquire().await;
        }
    }

    /// 串行语义钉：rate=60/min（1s/许可）时 61 张许可 = 首张即时 + 60 个整间隔。
    /// paused-time 下虚拟时钟恰走 60s——每张许可精确消费一个间隔，无失约、无叠加。
    #[tokio::test(start_paused = true)]
    async fn serial_acquire_consumes_exact_intervals() {
        let throttle = Throttle::limited(60);
        let t0 = Instant::now();
        for _ in 0..61 {
            throttle.acquire().await;
        }
        assert_eq!(
            t0.elapsed(),
            Duration::from_secs(60),
            "61 permits at 1/s must take exactly 60 intervals of virtual time"
        );
    }

    /// 压缩语义钉（终裁 M≤0.40 的前提）：并发 N 张许可的总墙钟 = (N-1) 个间隔，
    /// 不许出现「每等待者各自睡一个完整间隔」的串行化失真；
    /// 同时预约时刻彼此错开，绝无同刻双放行。
    #[tokio::test(start_paused = true)]
    async fn concurrent_burst_pays_one_interval_not_n() {
        let throttle = std::sync::Arc::new(Throttle::limited(120)); // 0.5s/许可
        let t0 = Instant::now();
        let mut set = tokio::task::JoinSet::new();
        for _ in 0..4 {
            let throttle = throttle.clone();
            set.spawn(async move {
                throttle.acquire().await;
            });
        }
        while set.join_next().await.is_some() {}
        let elapsed = t0.elapsed();
        // 4 并发：首许可即时 + 3 个半秒间隔 = 1.5s；串行化失真会 ≥ 2.0s。
        assert_eq!(
            elapsed,
            Duration::from_millis(1500),
            "burst of 4 at 120rpm must cost exactly 3 intervals"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn non_positive_rate_is_disabled() {
        for rate in [0, -5, i64::MIN] {
            let throttle = Throttle::limited(rate);
            for _ in 0..100 {
                throttle.acquire().await;
            }
        }
    }

    /// 真实时钟守恒：21 张许可 @120_000/min（0.5ms/许可）⇒ ≥ 20 个间隔（10ms）。
    /// 下界钉死「许可不可早于间隔放行」——滑窗限速的形式化腹稿。
    #[tokio::test]
    async fn window_real_time_at_least_n_minus_one_intervals() {
        let throttle = Throttle::limited(120_000); // 0.5ms/许可
        let t0 = std::time::Instant::now();
        for _ in 0..21 {
            throttle.acquire().await;
        }
        assert!(
            t0.elapsed() >= Duration::from_millis(10),
            "21 permits at 0.5ms interval must take >= 20 intervals (10ms), got {:?}",
            t0.elapsed()
        );
    }
}
