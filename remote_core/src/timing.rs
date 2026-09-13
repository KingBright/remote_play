use protocol::FrameTimingCheckpoints;
use quanta::{Clock, Instant};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// 高精度微秒时钟与锚点生成器 (基于 quanta 硬件 TSC，单次读取开销 8~18ns)
#[derive(Clone, Debug)]
pub struct HighPrecisionClock {
    clock: Clock,
    anchor_instant: Instant,
    anchor_epoch_us: u64,
}

impl HighPrecisionClock {
    /// 初始化时钟锚点
    pub fn new() -> Self {
        let clock = Clock::new();
        let anchor_instant = clock.now();
        let anchor_epoch_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;

        Self {
            clock,
            anchor_instant,
            anchor_epoch_us,
        }
    }

    /// 获取硬件单调瞬时点 (耗时 ~10ns)
    #[inline(always)]
    pub fn now(&self) -> Instant {
        self.clock.now()
    }

    /// 获取当前挂钟微秒绝对时间戳
    #[inline(always)]
    pub fn now_epoch_us(&self) -> u64 {
        let now_inst = self.clock.now();
        let delta = now_inst.saturating_duration_since(self.anchor_instant);
        self.anchor_epoch_us + delta.as_micros() as u64
    }

    /// 计算两个瞬时点之间的微秒差值 (Saturating)
    #[inline(always)]
    pub fn delta_us(&self, start: Instant, end: Instant) -> u32 {
        end.saturating_duration_since(start).as_micros() as u32
    }

    /// 计算自起始瞬时点至当前的微秒差值
    #[inline(always)]
    pub fn delta_since_us(&self, start: Instant) -> u32 {
        self.delta_us(start, self.now())
    }
}

impl Default for HighPrecisionClock {
    fn default() -> Self {
        Self::new()
    }
}

static GLOBAL_CLOCK: OnceLock<HighPrecisionClock> = OnceLock::new();

/// 获取全局共享的高精度时钟引用
#[inline]
pub fn global_clock() -> &'static HighPrecisionClock {
    GLOBAL_CLOCK.get_or_init(HighPrecisionClock::new)
}

/// 获取当前全局高精度微秒绝对时间戳 (Unix Epoch)
#[inline(always)]
pub fn quanta_now_us() -> u64 {
    global_clock().now_epoch_us()
}

/// 获取当前全局硬件单调瞬时点
#[inline(always)]
pub fn quanta_now() -> Instant {
    global_clock().now()
}

/// Host 端帧打点追踪器 (Zero Allocation, 栈分配)
pub struct HostFrameTracker {
    clock: HighPrecisionClock,
    capture_instant: Instant,
    pub checkpoints: FrameTimingCheckpoints,
}

impl HostFrameTracker {
    /// 启动 Host 帧捕获打点
    #[inline(always)]
    pub fn start(clock: HighPrecisionClock) -> Self {
        let capture_instant = clock.now();
        let capture_ts_us = clock.now_epoch_us();
        Self {
            clock,
            capture_instant,
            checkpoints: FrameTimingCheckpoints::new(capture_ts_us),
        }
    }

    /// 使用已有或外部指定基准时间戳初始化追踪器
    #[inline(always)]
    pub fn with_checkpoints(
        checkpoints: FrameTimingCheckpoints,
        clock: HighPrecisionClock,
    ) -> Self {
        let capture_instant = clock.now();
        Self {
            clock,
            capture_instant,
            checkpoints,
        }
    }

    /// 标记进入编码队列 S2a
    #[inline(always)]
    pub fn mark_encode_queue(&mut self) {
        let now = self.clock.now();
        self.checkpoints.encode_queue_ts_us = self.clock.delta_us(self.capture_instant, now);
    }

    /// 标记硬件编码完成 S2b
    #[inline(always)]
    pub fn mark_encode_done(&mut self) {
        let now = self.clock.now();
        self.checkpoints.encode_done_ts_us = self.clock.delta_us(self.capture_instant, now);
    }

    /// 标记分片封包完成 S3a
    #[inline(always)]
    pub fn mark_packetize(&mut self) {
        let now = self.clock.now();
        self.checkpoints.packetize_ts_us = self.clock.delta_us(self.capture_instant, now);
    }

    /// 标记 Socket 发射完成 S3b
    #[inline(always)]
    pub fn mark_send(&mut self) {
        let now = self.clock.now();
        self.checkpoints.send_ts_us = self.clock.delta_us(self.capture_instant, now);
    }

    /// 完成 Host 侧打点，产出 checkpoints
    #[inline(always)]
    pub fn finish(self) -> FrameTimingCheckpoints {
        self.checkpoints
    }
}

/// Host_Time ≈ Client_Time + `clock_offset_us`.
///
/// Returns a capture-relative offset. Never subtract a client wall-clock
/// sample from `capture_ts_us` directly: that saturates to 0 when the client
/// clock is behind the host, and inflates e2e when the client is ahead.
#[inline]
pub fn client_stage_offset_us(capture_ts_us: u64, client_now_us: u64, clock_offset_us: i64) -> u32 {
    aligned_client_offset_us(capture_ts_us, client_now_us, clock_offset_us, 0)
}

#[inline]
fn aligned_client_offset_us(
    capture_ts_us: u64,
    client_now_us: u64,
    clock_offset_us: i64,
    min_offset_us: u32,
) -> u32 {
    let min_epoch = capture_ts_us.saturating_add(min_offset_us as u64);
    let host_aligned = (client_now_us as i64)
        .saturating_add(clock_offset_us)
        .max(min_epoch as i64) as u64;
    (host_aligned.saturating_sub(capture_ts_us) as u32).max(min_offset_us)
}

/// Stamp a later client stage in the host-capture domain.
///
/// Uses Ping/Pong `clock_offset_us` when available, and never moves earlier
/// than recv/jitter/decode checkpoints already on `timing`.
#[inline]
pub fn stamp_client_stage(
    timing: &FrameTimingCheckpoints,
    client_now_us: u64,
    clock_offset_us: i64,
) -> u32 {
    client_stage_offset_us(timing.capture_ts_us, client_now_us, clock_offset_us)
        .max(timing.recv_ts_us)
        .max(timing.jitter_enter_ts_us)
        .max(timing.jitter_exit_ts_us)
        .max(timing.decode_enter_ts_us)
        .max(timing.decode_done_ts_us)
        .max(timing.render_submit_ts_us)
        .max(timing.render_done_ts_us)
}

/// Advance a previously host-aligned client stage by local elapsed time.
/// This is the correct S6/S7/S8 path when `recv_ts_us` is already aligned.
#[inline]
pub fn advance_client_stage(previous_stage_us: u32, elapsed_us: u32) -> u32 {
    previous_stage_us.saturating_add(elapsed_us)
}

/// Client 端帧打点追踪器 (Zero Allocation, 栈分配)
pub struct ClientFrameTracker {
    clock: HighPrecisionClock,
    recv_instant: Instant,
    pub checkpoints: FrameTimingCheckpoints,
}

impl ClientFrameTracker {
    /// 基于从 Host 接收到的时戳与网络时钟偏移，初始化 Client 侧追踪
    #[inline(always)]
    pub fn start(
        host_checkpoints: FrameTimingCheckpoints,
        clock: HighPrecisionClock,
        clock_offset_us: i64,
    ) -> Self {
        let recv_instant = clock.now();
        let client_recv_epoch_us = clock.now_epoch_us();

        let mut checkpoints = host_checkpoints;
        checkpoints.recv_ts_us = aligned_client_offset_us(
            host_checkpoints.capture_ts_us,
            client_recv_epoch_us,
            clock_offset_us,
            host_checkpoints.send_ts_us,
        );

        Self {
            clock,
            recv_instant,
            checkpoints,
        }
    }

    /// 使用已有包含 recv_ts_us 的 checkpoints 初始化 Client 追踪器
    #[inline(always)]
    pub fn with_recv(checkpoints: FrameTimingCheckpoints, clock: HighPrecisionClock) -> Self {
        let recv_instant = clock.now();
        Self {
            clock,
            recv_instant,
            checkpoints,
        }
    }

    /// 标记进入 Jitter Buffer S5 -> S6
    #[inline(always)]
    pub fn mark_jitter_enter(&mut self) {
        let delta_from_recv = self.clock.delta_since_us(self.recv_instant);
        self.checkpoints.jitter_enter_ts_us =
            self.checkpoints.recv_ts_us.saturating_add(delta_from_recv);
    }

    /// 标记从 Jitter Buffer 出队 S6
    #[inline(always)]
    pub fn mark_jitter_exit(&mut self) {
        let delta_from_recv = self.clock.delta_since_us(self.recv_instant);
        self.checkpoints.jitter_exit_ts_us =
            self.checkpoints.recv_ts_us.saturating_add(delta_from_recv);
    }

    /// 标记提交硬件解码器 S7a
    #[inline(always)]
    pub fn mark_decode_enter(&mut self) {
        let delta_from_recv = self.clock.delta_since_us(self.recv_instant);
        self.checkpoints.decode_enter_ts_us =
            self.checkpoints.recv_ts_us.saturating_add(delta_from_recv);
    }

    /// 标记硬件解码完成 S7b
    #[inline(always)]
    pub fn mark_decode_done(&mut self) {
        let delta_from_recv = self.clock.delta_since_us(self.recv_instant);
        self.checkpoints.decode_done_ts_us =
            self.checkpoints.recv_ts_us.saturating_add(delta_from_recv);
    }

    /// 标记提交 GPU Metal 渲染管线 S8a
    #[inline(always)]
    pub fn mark_render_submit(&mut self) {
        let delta_from_recv = self.clock.delta_since_us(self.recv_instant);
        self.checkpoints.render_submit_ts_us =
            self.checkpoints.recv_ts_us.saturating_add(delta_from_recv);
    }

    /// 标记渲染呈现完成 (VSync 屏幕显示) S8b
    #[inline(always)]
    pub fn mark_render_done(&mut self) {
        let delta_from_recv = self.clock.delta_since_us(self.recv_instant);
        self.checkpoints.render_done_ts_us =
            self.checkpoints.recv_ts_us.saturating_add(delta_from_recv);
    }

    /// 完成全部阶段打点
    #[inline(always)]
    pub fn finish(self) -> FrameTimingCheckpoints {
        self.checkpoints
    }
}

/// 时钟同步与 RTT 偏移估计器 (基于 Ping/Pong 与 EWMA 滤波)
#[derive(Debug, Clone)]
pub struct ClockSynchronizer {
    rtt_us: f64,
    clock_offset_us: f64,
    initialized: bool,
}

impl ClockSynchronizer {
    pub fn new() -> Self {
        Self {
            rtt_us: 0.0,
            clock_offset_us: 0.0,
            initialized: false,
        }
    }

    /// 处理 Ping-Pong 消息并更新估算
    /// t1: client_send_ts (微秒)
    /// t2: host_recv_ts (微秒)
    /// t3: host_send_ts (微秒)
    /// t4: client_recv_ts (微秒)
    pub fn update_pong(&mut self, t1: u64, t2: u64, t3: u64, t4: u64) {
        if t4 <= t1 || t3 < t2 {
            return;
        }
        let total_rtt = (t4 - t1) as f64;
        let host_processing = (t3 - t2) as f64;
        let sample_rtt = (total_rtt - host_processing).max(0.0);

        // Host_Time = Client_Time + Offset
        // Offset = ((t2 - t1) + (t3 - t4)) / 2
        let sample_offset = (((t2 as i64 - t1 as i64) + (t3 as i64 - t4 as i64)) as f64) / 2.0;

        if !self.initialized {
            self.rtt_us = sample_rtt;
            self.clock_offset_us = sample_offset;
            self.initialized = true;
        } else {
            // 剔除偏离当前均值 3 倍以上的异常峰值
            if sample_rtt < self.rtt_us * 3.0 || self.rtt_us < 1_000.0 {
                self.rtt_us = self.rtt_us * 0.85 + sample_rtt * 0.15;
                self.clock_offset_us = self.clock_offset_us * 0.85 + sample_offset * 0.15;
            }
        }
    }

    #[inline]
    pub fn clock_offset_us(&self) -> i64 {
        self.clock_offset_us as i64
    }

    #[inline]
    pub fn rtt_us(&self) -> u32 {
        self.rtt_us as u32
    }

    #[inline]
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }
}

impl Default for ClockSynchronizer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_high_precision_clock_monotonicity() {
        let clock = HighPrecisionClock::new();
        let t1 = clock.now();
        let us1 = clock.now_epoch_us();
        assert!(us1 > 1_700_000_000_000_000); // Beyond 2023

        let mut prev = clock.now();
        for _ in 0..100 {
            let curr = clock.now();
            assert!(curr >= prev);
            prev = curr;
        }
        let t2 = clock.now();
        let delta = clock.delta_us(t1, t2);
        assert!(delta < 1_000_000); // Sub-second
    }

    #[test]
    fn test_host_frame_tracker_workflow() {
        let clock = HighPrecisionClock::new();
        let mut tracker = HostFrameTracker::start(clock);
        assert!(tracker.checkpoints.capture_ts_us > 0);

        tracker.mark_encode_queue();
        assert!(tracker.checkpoints.encode_queue_ts_us <= tracker.checkpoints.encode_done_ts_us);

        tracker.mark_encode_done();
        tracker.mark_packetize();
        tracker.mark_send();

        let cp = tracker.finish();
        assert!(cp.send_ts_us >= cp.encode_done_ts_us);
        assert!(cp.encode_done_ts_us >= cp.encode_queue_ts_us);
    }

    #[test]
    fn test_client_frame_tracker_workflow() {
        let clock = HighPrecisionClock::new();
        let mut host_cp = FrameTimingCheckpoints::new(clock.now_epoch_us().saturating_sub(10_000));
        host_cp.encode_queue_ts_us = 500;
        host_cp.encode_done_ts_us = 2_000;
        host_cp.send_ts_us = 2_500;

        let mut tracker = ClientFrameTracker::start(host_cp, clock, 0);
        tracker.mark_jitter_enter();
        tracker.mark_jitter_exit();
        tracker.mark_decode_enter();
        tracker.mark_decode_done();
        tracker.mark_render_submit();
        tracker.mark_render_done();

        let cp = tracker.finish();
        assert!(cp.recv_ts_us >= cp.send_ts_us);
        assert!(cp.jitter_enter_ts_us >= cp.recv_ts_us);
        assert!(cp.jitter_exit_ts_us >= cp.jitter_enter_ts_us);
        assert!(cp.decode_enter_ts_us >= cp.jitter_exit_ts_us);
        assert!(cp.decode_done_ts_us >= cp.decode_enter_ts_us);
        assert!(cp.render_submit_ts_us >= cp.decode_done_ts_us);
        assert!(cp.render_done_ts_us >= cp.render_submit_ts_us);
    }

    #[test]
    fn client_stage_offset_survives_client_clock_behind_host() {
        let capture_ts_us = 1_700_000_000_000_000u64;
        let client_behind_us = 5_000_000i64;
        let true_delay_us = 12_000u32;
        let client_now_us = capture_ts_us - client_behind_us as u64 + true_delay_us as u64;
        let clock_offset_us = client_behind_us;

        let naive = client_now_us.saturating_sub(capture_ts_us) as u32;
        assert_eq!(naive, 0, "naive wall-clock mix is the production bug");

        let aligned = client_stage_offset_us(capture_ts_us, client_now_us, clock_offset_us);
        assert_eq!(aligned, true_delay_us);
    }

    #[test]
    fn client_stage_offset_does_not_inflate_when_client_clock_is_ahead() {
        let capture_ts_us = 1_700_000_000_000_000u64;
        let client_ahead_us = 5_000_000i64;
        let true_delay_us = 12_000u32;
        let client_now_us = capture_ts_us + client_ahead_us as u64 + true_delay_us as u64;
        let clock_offset_us = -client_ahead_us;

        let naive = client_now_us.saturating_sub(capture_ts_us) as u32;
        assert_eq!(naive, 5_012_000, "naive mix inflates e2e by the clock skew");

        let aligned = client_stage_offset_us(capture_ts_us, client_now_us, clock_offset_us);
        assert_eq!(aligned, true_delay_us);
    }

    #[test]
    fn stamp_client_stage_stays_monotonic_with_recv() {
        let mut timing = FrameTimingCheckpoints::new(1_700_000_000_000_000);
        timing.send_ts_us = 2_500;
        timing.recv_ts_us = 10_000;
        timing.jitter_exit_ts_us = 13_000;

        let client_now_us = timing.capture_ts_us - 4_000_000;
        let stamped = stamp_client_stage(&timing, client_now_us, 0);
        assert_eq!(stamped, 13_000);
        assert!(stamped >= timing.recv_ts_us);
        assert!(stamped >= timing.jitter_exit_ts_us);
    }

    #[test]
    fn advance_client_stage_keeps_host_aligned_domain() {
        assert_eq!(advance_client_stage(13_000, 4_500), 17_500);
        assert_eq!(advance_client_stage(u32::MAX - 10, 50), u32::MAX);
    }

    #[test]
    fn test_clock_synchronizer_update_and_outliers() {
        let mut syncer = ClockSynchronizer::new();
        assert!(!syncer.is_initialized());

        // t1: 1000, t2: 1005 (host received), t3: 1006 (host replied), t4: 1012 (client received)
        // total_rtt = 1012 - 1000 = 12, host_proc = 1
        // sample_rtt = 11
        // sample_offset = ((1005 - 1000) + (1006 - 1012)) / 2 = (5 + (-6)) / 2 = -0.5
        syncer.update_pong(1_000_000, 1_005_000, 1_006_000, 1_012_000);
        assert!(syncer.is_initialized());
        assert_eq!(syncer.rtt_us(), 11_000);
        assert_eq!(syncer.clock_offset_us(), -500);

        // Update with another normal sample
        syncer.update_pong(2_000_000, 2_005_000, 2_006_000, 2_012_000);
        assert_eq!(syncer.rtt_us(), 11_000);

        // Huge spike outlier (RTT = 100ms)
        syncer.update_pong(3_000_000, 3_055_000, 3_056_000, 3_112_000);
        // Outlier filtered, RTT should not jump to 100ms
        assert!(syncer.rtt_us() < 30_000);
    }
}
