use protocol::{FrameTimingCheckpoints, PipelineTelemetryReport, StageId, StageLatencyStats};
use std::collections::VecDeque;
use std::fmt;
use std::time::{Duration, Instant};

/// 滚动多分位数聚合器 (常数空间滑动窗口，支持精确计算 p50/p95/p99/min/max/avg/stddev)
#[derive(Debug, Clone)]
pub struct RollingQuantileAggregator {
    window_size: usize,
    samples: Vec<u32>,
    sorted_samples: Vec<u32>,
    head: usize,
    count: usize,
    sum: u64,
    sum_squares: u128,
}

impl RollingQuantileAggregator {
    /// 创建指定容量的滑动窗口聚合器 (默认推荐 120~300 个样本，约覆盖 2~5 秒 60fps 数据)
    pub fn new(window_size: usize) -> Self {
        let cap = window_size.max(1);
        Self {
            window_size: cap,
            samples: Vec::with_capacity(cap),
            sorted_samples: Vec::with_capacity(cap),
            head: 0,
            count: 0,
            sum: 0,
            sum_squares: 0,
        }
    }

    /// 零额外堆分配录入一个微秒耗时样本，并同步维护有序窗口。
    ///
    /// 窗口很小（通常 120~300），用连续内存中的二分定位 + memmove 换取
    /// 报告阶段 O(1) 的精确分位数读取，避免周期性 clone/sort 抖动进入媒体热路径。
    #[inline]
    pub fn record(&mut self, val_us: u32) {
        let val_sq = (val_us as u128) * (val_us as u128);
        if self.samples.len() < self.window_size {
            self.samples.push(val_us);
            self.sum = self.sum.saturating_add(val_us as u64);
            self.sum_squares = self.sum_squares.saturating_add(val_sq);
        } else {
            let old_val = self.samples[self.head];
            let old_sq = (old_val as u128) * (old_val as u128);
            self.sum = self
                .sum
                .saturating_sub(old_val as u64)
                .saturating_add(val_us as u64);
            self.sum_squares = self
                .sum_squares
                .saturating_sub(old_sq)
                .saturating_add(val_sq);
            self.samples[self.head] = val_us;
            self.head = (self.head + 1) % self.window_size;
            self.remove_sorted(old_val);
        }
        self.insert_sorted(val_us);
        self.count = self.count.saturating_add(1);
    }

    #[inline]
    fn insert_sorted(&mut self, val_us: u32) {
        let idx = self
            .sorted_samples
            .partition_point(|&sample| sample <= val_us);
        self.sorted_samples.insert(idx, val_us);
    }

    #[inline]
    fn remove_sorted(&mut self, val_us: u32) {
        let idx = self
            .sorted_samples
            .partition_point(|&sample| sample < val_us);
        debug_assert_eq!(self.sorted_samples.get(idx), Some(&val_us));
        self.sorted_samples.remove(idx);
    }

    /// 获取当前窗口内样本数
    #[inline]
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// 判断当前窗口是否为空
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// 获取历史总样本计数
    #[inline]
    pub fn total_count(&self) -> usize {
        self.count
    }

    /// 重置所有样本
    pub fn reset(&mut self) {
        self.samples.clear();
        self.sorted_samples.clear();
        self.head = 0;
        self.count = 0;
        self.sum = 0;
        self.sum_squares = 0;
    }

    /// 生成当前窗口的完整分位数与统计指标快照
    pub fn snapshot(&self) -> StageLatencyStats {
        let n = self.samples.len();
        if n == 0 {
            return StageLatencyStats::default();
        }

        debug_assert_eq!(n, self.sorted_samples.len());
        let sorted = &self.sorted_samples;
        let min_us = sorted[0];
        let max_us = sorted[n - 1];

        // 最近邻阶梯插值 (Nearest Rank Method)。有序窗口在 record() 时增量维护，
        // 因此报告阶段不再 clone/sort，也不会产生瞬时堆分配。
        let p50_idx = (n * 50).div_ceil(100) - 1;
        let p95_idx = (n * 95).div_ceil(100) - 1;
        let p99_idx = (n * 99).div_ceil(100) - 1;

        let p50_us = sorted[p50_idx];
        let p95_us = sorted[p95_idx];
        let p99_us = sorted[p99_idx];

        let avg_us = (self.sum / (n as u64)) as u32;
        let mean = self.sum as f64 / n as f64;
        let mean_square = self.sum_squares as f64 / n as f64;
        let variance = (mean_square - mean * mean).max(0.0);
        let stddev_us = variance.sqrt().round() as u32;

        StageLatencyStats {
            min_us,
            p50_us,
            p95_us,
            p99_us,
            max_us,
            avg_us,
            stddev_us,
            sample_count: n as u32,
        }
    }
}

impl Default for RollingQuantileAggregator {
    fn default() -> Self {
        Self::new(300)
    }
}

/// 丢帧/丢包原因分类
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DropReason {
    /// 迟到丢弃 (超过起搏时限)
    Late,
    /// 缓冲队列满溢丢弃
    QueueFull,
    /// 画面破损/解码错误丢弃
    Corrupt,
    /// 传输层丢包计数增量
    PacketLost(u64),
}

/// 滚动 FPS 计算器 (平滑滑动时间戳窗口)
#[derive(Debug, Clone)]
pub struct RollingFpsCalculator {
    timestamps: VecDeque<Instant>,
    window_duration: Duration,
}

impl RollingFpsCalculator {
    pub fn new(window_duration: Duration) -> Self {
        Self {
            timestamps: VecDeque::with_capacity(300),
            window_duration,
        }
    }

    pub fn record_tick(&mut self, now: Instant) {
        self.timestamps.push_back(now);
        let cutoff = now.checked_sub(self.window_duration).unwrap_or(now);
        while self.timestamps.front().is_some_and(|&ts| ts < cutoff) {
            self.timestamps.pop_front();
        }
    }

    pub fn current_fps(&self) -> f32 {
        self.fps_at(Instant::now())
    }

    fn fps_at(&self, now: Instant) -> f32 {
        if self
            .timestamps
            .back()
            .is_some_and(|&last| now.saturating_duration_since(last) > self.window_duration)
        {
            return 0.0;
        }
        let n = self.timestamps.len();
        if n < 2 {
            return if n == 1 { 1.0 } else { 0.0 };
        }
        let first = self.timestamps[0];
        let last = *self.timestamps.back().unwrap();
        let elapsed = last.saturating_duration_since(first).as_secs_f32();
        if elapsed > 0.001 {
            ((n - 1) as f32) / elapsed
        } else {
            0.0
        }
    }
}

impl Default for RollingFpsCalculator {
    fn default() -> Self {
        Self::new(Duration::from_secs(2))
    }
}

/// 滚动码率计算器 (比特率统计窗口)
#[derive(Debug, Clone)]
pub struct RollingBitrateCalculator {
    records: VecDeque<(Instant, usize)>,
    window_duration: Duration,
    total_bytes: u64,
}

impl RollingBitrateCalculator {
    pub fn new(window_duration: Duration) -> Self {
        Self {
            records: VecDeque::with_capacity(300),
            window_duration,
            total_bytes: 0,
        }
    }

    pub fn record_bytes(&mut self, bytes: usize, now: Instant) {
        self.records.push_back((now, bytes));
        self.total_bytes = self.total_bytes.saturating_add(bytes as u64);
        let cutoff = now.checked_sub(self.window_duration).unwrap_or(now);
        while self.records.front().is_some_and(|&(ts, _)| ts < cutoff) {
            if let Some((_, expired_bytes)) = self.records.pop_front() {
                self.total_bytes = self.total_bytes.saturating_sub(expired_bytes as u64);
            }
        }
    }

    pub fn current_bitrate_kbps(&self) -> u32 {
        let n = self.records.len();
        if n < 2 {
            return 0;
        }
        let first_ts = self.records.front().expect("non-empty bitrate window").0;
        let last_ts = self.records.back().expect("non-empty bitrate window").0;
        let elapsed = last_ts.saturating_duration_since(first_ts).as_secs_f32();
        if elapsed > 0.001 {
            let kbps = (self.total_bytes as f32 * 8.0) / (elapsed * 1000.0);
            kbps.round() as u32
        } else {
            0
        }
    }
}

impl Default for RollingBitrateCalculator {
    fn default() -> Self {
        Self::new(Duration::from_secs(2))
    }
}

/// 8 阶段全链路运行时遥测引擎
#[derive(Debug, Clone)]
pub struct PipelineTelemetryEngine {
    session_id: u32,
    stage_aggregators: [RollingQuantileAggregator; StageId::STAGE_COUNT],
    e2e_aggregator: RollingQuantileAggregator,
    fps_calculator: RollingFpsCalculator,
    bitrate_calculator: RollingBitrateCalculator,
    jitter_buffer_depth: usize,
    packets_lost: u64,
    late_frames_dropped: u64,
    queue_full_dropped: u64,
    corrupt_frames_dropped: u64,
}

impl PipelineTelemetryEngine {
    /// 创建新的全链路遥测引擎
    pub fn new(session_id: u32, window_size: usize) -> Self {
        Self {
            session_id,
            stage_aggregators: [
                RollingQuantileAggregator::new(window_size),
                RollingQuantileAggregator::new(window_size),
                RollingQuantileAggregator::new(window_size),
                RollingQuantileAggregator::new(window_size),
                RollingQuantileAggregator::new(window_size),
                RollingQuantileAggregator::new(window_size),
                RollingQuantileAggregator::new(window_size),
                RollingQuantileAggregator::new(window_size),
                RollingQuantileAggregator::new(window_size),
            ],
            e2e_aggregator: RollingQuantileAggregator::new(window_size),
            fps_calculator: RollingFpsCalculator::default(),
            bitrate_calculator: RollingBitrateCalculator::default(),
            jitter_buffer_depth: 0,
            packets_lost: 0,
            late_frames_dropped: 0,
            queue_full_dropped: 0,
            corrupt_frames_dropped: 0,
        }
    }

    /// 录入已完成全流程或部分流程的一帧时序检查点与数据体量
    pub fn record_frame(&mut self, checkpoints: &FrameTimingCheckpoints, frame_bytes: usize) {
        let now = Instant::now();
        self.fps_calculator.record_tick(now);
        if frame_bytes > 0 {
            self.bitrate_calculator.record_bytes(frame_bytes, now);
        }

        // 记录各个阶段的独立耗时
        for stage in StageId::ALL {
            if let Some(dur_us) = checkpoints.stage_duration_us(stage) {
                self.stage_aggregators[stage as usize].record(dur_us);
            }
        }

        // 记录端到端总时延
        if let Some(e2e_us) = checkpoints.total_e2e_duration_us() {
            self.e2e_aggregator.record(e2e_us);
        }
    }

    /// 录入丢包与丢帧事件
    pub fn record_drop(&mut self, reason: DropReason) {
        match reason {
            DropReason::Late => {
                self.late_frames_dropped = self.late_frames_dropped.saturating_add(1);
            }
            DropReason::QueueFull => {
                self.queue_full_dropped = self.queue_full_dropped.saturating_add(1);
            }
            DropReason::Corrupt => {
                self.corrupt_frames_dropped = self.corrupt_frames_dropped.saturating_add(1);
            }
            DropReason::PacketLost(count) => {
                self.packets_lost = self.packets_lost.saturating_add(count);
            }
        }
    }

    /// 更新当前 Jitter Buffer 队列深度
    pub fn set_jitter_buffer_depth(&mut self, depth: usize) {
        self.jitter_buffer_depth = depth;
    }

    /// 获取特定阶段的分位数聚合器引用
    pub fn stage_aggregator(&self, stage: StageId) -> &RollingQuantileAggregator {
        &self.stage_aggregators[stage as usize]
    }

    /// 获取端到端分位数聚合器引用
    pub fn e2e_aggregator(&self) -> &RollingQuantileAggregator {
        &self.e2e_aggregator
    }

    /// 生成即时全链路遥测报告
    pub fn generate_report(&self) -> PipelineTelemetryReport {
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let mut stage_stats = [StageLatencyStats::default(); StageId::STAGE_COUNT];
        for (i, stat) in stage_stats.iter_mut().enumerate() {
            *stat = self.stage_aggregators[i].snapshot();
        }

        PipelineTelemetryReport {
            session_id: self.session_id,
            timestamp_ms,
            fps: self.fps_calculator.current_fps(),
            bitrate_kbps: self.bitrate_calculator.current_bitrate_kbps(),
            stage_stats,
            e2e_stats: self.e2e_aggregator.snapshot(),
            jitter_buffer_depth: self.jitter_buffer_depth,
            packets_lost: self.packets_lost,
            late_frames_dropped: self.late_frames_dropped,
            queue_full_dropped: self.queue_full_dropped,
            corrupt_frames_dropped: self.corrupt_frames_dropped,
        }
    }

    /// 生成 ASCII 格式的结构化遥测诊断表格
    pub fn format_ascii_summary_table(&self) -> String {
        let report = self.generate_report();
        let mut out = String::with_capacity(1024);

        out.push_str(&format!(
            "┌─────────────────────────────────────────────────────────────────────────────────────────────┐\n\
             │ PIPELINE TELEMETRY REPORT [Session: {:<8}] FPS: {:>5.1} | Bitrate: {:>6} kbps | JBuf: {:>2}   │\n\
             ├──────────────────────┬──────────┬──────────┬──────────┬──────────┬──────────┬──────────────┤\n\
             │ Stage Name           │   Min    │   p50    │   p95    │   p99    │   Max    │ StdDev / Avg │\n\
             ├──────────────────────┼──────────┼──────────┼──────────┼──────────┼──────────┼──────────────┤\n",
            report.session_id, report.fps, report.bitrate_kbps, report.jitter_buffer_depth
        ));

        for stage in StageId::ALL {
            let stats = &report.stage_stats[stage as usize];
            let name_label = format!("{:<20}", stage.name());
            if stats.sample_count > 0 {
                out.push_str(&format!(
                    "│ {} │ {:>6.2}ms │ {:>6.2}ms │ {:>6.2}ms │ {:>6.2}ms │ {:>6.2}ms │ {:>4.1} / {:>4.1}ms │\n",
                    name_label,
                    stats.min_us as f64 / 1000.0,
                    stats.p50_us as f64 / 1000.0,
                    stats.p95_us as f64 / 1000.0,
                    stats.p99_us as f64 / 1000.0,
                    stats.max_us as f64 / 1000.0,
                    stats.stddev_us as f64 / 1000.0,
                    stats.avg_us as f64 / 1000.0,
                ));
            } else {
                out.push_str(&format!(
                    "│ {} │    N/A   │    N/A   │    N/A   │    N/A   │    N/A   │     N/A      │\n",
                    name_label
                ));
            }
        }

        out.push_str("├──────────────────────┼──────────┼──────────┼──────────┼──────────┼──────────┼──────────────┤\n");
        let e2e = &report.e2e_stats;
        if e2e.sample_count > 0 {
            out.push_str(&format!(
                "│ End-to-End Latency   │ {:>6.2}ms │ {:>6.2}ms │ {:>6.2}ms │ {:>6.2}ms │ {:>6.2}ms │ {:>4.1} / {:>4.1}ms │\n",
                e2e.min_us as f64 / 1000.0,
                e2e.p50_us as f64 / 1000.0,
                e2e.p95_us as f64 / 1000.0,
                e2e.p99_us as f64 / 1000.0,
                e2e.max_us as f64 / 1000.0,
                e2e.stddev_us as f64 / 1000.0,
                e2e.avg_us as f64 / 1000.0,
            ));
        } else {
            out.push_str("│ End-to-End Latency   │    N/A   │    N/A   │    N/A   │    N/A   │    N/A   │     N/A      │\n");
        }

        out.push_str(&format!(
            "└──────────────────────┴──────────┴──────────┴──────────┴──────────┴──────────┴──────────────┘\n\
             [Drops] Late: {} | QueueFull: {} | Corrupt: {} | PktLoss: {}\n",
            report.late_frames_dropped,
            report.queue_full_dropped,
            report.corrupt_frames_dropped,
            report.packets_lost
        ));

        out
    }
}

impl fmt::Display for PipelineTelemetryEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.format_ascii_summary_table())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_percentiles_use_nearest_rank_for_small_windows() {
        let mut agg = RollingQuantileAggregator::new(10);
        for value in 1..=10 {
            agg.record(value);
        }
        let stats = agg.snapshot();
        assert_eq!((stats.p50_us, stats.p95_us, stats.p99_us), (5, 10, 10));
    }

    #[test]
    fn fps_expires_when_frames_stop_arriving() {
        let now = Instant::now();
        let mut fps = RollingFpsCalculator::new(Duration::from_secs(1));
        fps.record_tick(now);
        fps.record_tick(now + Duration::from_millis(10));
        assert_eq!(fps.fps_at(now + Duration::from_millis(10)), 100.0);
        assert_eq!(fps.fps_at(now + Duration::from_secs(2)), 0.0);
    }

    #[test]
    fn test_rolling_quantile_empty() {
        let agg = RollingQuantileAggregator::new(100);
        assert_eq!(agg.len(), 0);
        assert!(agg.is_empty());
        let stats = agg.snapshot();
        assert_eq!(stats.sample_count, 0);
        assert_eq!(stats.p50_us, 0);
        assert_eq!(stats.min_us, 0);
        assert_eq!(stats.max_us, 0);
    }

    #[test]
    fn test_rolling_quantile_single_sample() {
        let mut agg = RollingQuantileAggregator::new(100);
        agg.record(5_000);
        assert_eq!(agg.len(), 1);
        let stats = agg.snapshot();
        assert_eq!(stats.sample_count, 1);
        assert_eq!(stats.min_us, 5_000);
        assert_eq!(stats.p50_us, 5_000);
        assert_eq!(stats.p95_us, 5_000);
        assert_eq!(stats.p99_us, 5_000);
        assert_eq!(stats.max_us, 5_000);
        assert_eq!(stats.avg_us, 5_000);
        assert_eq!(stats.stddev_us, 0);
    }

    #[test]
    fn test_rolling_quantile_accuracy() {
        let mut agg = RollingQuantileAggregator::new(100);
        // Record 1 to 100 ms (1000 to 100_000 us)
        for i in 1..=100 {
            agg.record(i * 1000);
        }

        let stats = agg.snapshot();
        assert_eq!(stats.sample_count, 100);
        assert_eq!(stats.min_us, 1_000);
        assert_eq!(stats.p50_us, 50_000);
        assert_eq!(stats.p95_us, 95_000);
        assert_eq!(stats.p99_us, 99_000);
        assert_eq!(stats.max_us, 100_000);
        assert_eq!(stats.avg_us, 50_500);
        // Std dev of uniform 1..100 is ~28.86 ms (~28866 us)
        assert!((stats.stddev_us as i32 - 28_866).abs() < 100);
    }

    #[test]
    fn test_rolling_quantile_window_wrap() {
        let mut agg = RollingQuantileAggregator::new(10);
        for i in 1..=10 {
            agg.record(i * 100);
        }
        assert_eq!(agg.len(), 10);
        assert_eq!(agg.snapshot().min_us, 100);

        // Push 10 more elements (11..20), all original 1..10 should be evicted
        for i in 11..=20 {
            agg.record(i * 100);
        }
        assert_eq!(agg.len(), 10);
        let stats = agg.snapshot();
        assert_eq!(stats.min_us, 1100);
        assert_eq!(stats.max_us, 2000);
        assert_eq!(agg.total_count(), 20);
    }

    #[test]
    fn test_pipeline_telemetry_engine_full_workflow() {
        let mut engine = PipelineTelemetryEngine::new(42, 100);

        let mut cp1 = FrameTimingCheckpoints::new(1_000_000);
        cp1.encode_queue_ts_us = 1_000;
        cp1.encode_done_ts_us = 4_000;
        cp1.packetize_ts_us = 4_500;
        cp1.send_ts_us = 5_000;
        cp1.recv_ts_us = 12_000;
        cp1.jitter_enter_ts_us = 12_500;
        cp1.jitter_exit_ts_us = 15_000;
        cp1.decode_enter_ts_us = 15_500;
        cp1.decode_done_ts_us = 18_000;
        cp1.render_submit_ts_us = 18_500;
        cp1.render_done_ts_us = 20_000;

        engine.record_frame(&cp1, 15_000);
        engine.record_drop(DropReason::Late);
        engine.record_drop(DropReason::QueueFull);
        engine.record_drop(DropReason::PacketLost(3));
        engine.set_jitter_buffer_depth(4);

        let report = engine.generate_report();
        assert_eq!(report.session_id, 42);
        assert_eq!(report.jitter_buffer_depth, 4);
        assert_eq!(report.late_frames_dropped, 1);
        assert_eq!(report.queue_full_dropped, 1);
        assert_eq!(report.packets_lost, 3);
        assert_eq!(report.stage_stats[StageId::Capture as usize].min_us, 1_000);
        assert_eq!(
            report.stage_stats[StageId::HardwareEncode as usize].min_us,
            3_000
        );
        assert_eq!(
            report.stage_stats[StageId::NetworkTransit as usize].min_us,
            7_000
        );
        assert_eq!(
            report.stage_stats[StageId::HardwareDecode as usize].min_us,
            2_500
        );
        assert_eq!(
            report.stage_stats[StageId::RenderPresentation as usize].min_us,
            1_500
        );
        assert_eq!(report.e2e_stats.min_us, 20_000);

        let table = engine.format_ascii_summary_table();
        assert!(table.contains("PIPELINE TELEMETRY REPORT"));
        assert!(table.contains("Capture"));
        assert!(table.contains("NetworkTransit"));
        assert!(table.contains("HardwareDecode"));
        assert!(table.contains("End-to-End Latency"));
    }
}
