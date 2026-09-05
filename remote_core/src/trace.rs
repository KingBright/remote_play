use protocol::{
    FrameTimingCheckpoints, PipelineTelemetryReport, StageId, StageLatencyStats, TraceSpan,
};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fmt;
use std::fs::File;
use std::io::Write;
use std::path::Path;

/// Chrome Trace 完整导出包装结构体
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChromeTraceDocument {
    #[serde(rename = "traceEvents")]
    pub trace_events: Vec<TraceSpan>,
    #[serde(rename = "displayTimeUnit", skip_serializing_if = "Option::is_none")]
    pub display_time_unit: Option<String>,
}

/// Chrome Trace JSON 格式化与导出器 (兼容 chrome://tracing 与 Perfetto)
pub struct ChromeTraceExporter;

impl ChromeTraceExporter {
    /// 将 TraceSpan 数组导出为标准 Chrome Trace Event JSON 字符串
    pub fn export_chrome_trace(spans: &[TraceSpan]) -> String {
        let doc = ChromeTraceDocument {
            trace_events: spans.to_vec(),
            display_time_unit: Some("ms".to_string()),
        };
        serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{\"traceEvents\":[]}".to_string())
    }

    /// 将多帧时序检查点直接转换为 Chrome Trace Event JSON 字符串
    pub fn export_frames_trace(
        frames: &[(u64, FrameTimingCheckpoints)],
        session_id: u32,
    ) -> String {
        let mut all_spans = Vec::with_capacity(frames.len() * StageId::STAGE_COUNT);
        for &(frame_id, ref checkpoints) in frames {
            let spans = checkpoints.to_trace_spans(frame_id, session_id);
            all_spans.extend(spans);
        }
        Self::export_chrome_trace(&all_spans)
    }

    /// 将 TraceSpan 写入指定本地文件
    pub fn export_to_file(spans: &[TraceSpan], path: impl AsRef<Path>) -> std::io::Result<()> {
        let json_str = Self::export_chrome_trace(spans);
        let mut file = File::create(path)?;
        file.write_all(json_str.as_bytes())?;
        file.flush()
    }
}

/// 时延毛刺触发捕获事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpikeReport {
    pub spike_frame_id: u64,
    pub total_e2e_us: u32,
    pub threshold_us: u32,
    pub culprit_stage: StageId,
    pub culprit_duration_us: u32,
    pub trace_json: String,
    pub timestamp_ms: u64,
}

/// 时延毛刺自动触发器 (Spike Trigger)
pub struct TraceSpikeTrigger {
    session_id: u32,
    threshold_us: u32,
    pre_spike_capacity: usize,
    history: VecDeque<(u64, FrameTimingCheckpoints)>,
    last_spike: Option<SpikeReport>,
}

impl TraceSpikeTrigger {
    /// 创建新的毛刺触发器 (threshold_us: 微秒阈值，例如 50_000 表示 50ms)
    pub fn new(session_id: u32, threshold_us: u32, pre_spike_capacity: usize) -> Self {
        let cap = pre_spike_capacity.max(10);
        Self {
            session_id,
            threshold_us,
            pre_spike_capacity: cap,
            history: VecDeque::with_capacity(cap + 1),
            last_spike: None,
        }
    }

    /// 获取配置的阈值（微秒）
    pub fn threshold_us(&self) -> u32 {
        self.threshold_us
    }

    /// 更新时延阈值
    pub fn set_threshold_us(&mut self, threshold_us: u32) {
        self.threshold_us = threshold_us;
    }

    /// 录入一帧检查点，若超出阈值则自动捕获并生成 SpikeReport
    pub fn record_frame(
        &mut self,
        frame_id: u64,
        checkpoints: FrameTimingCheckpoints,
    ) -> Option<SpikeReport> {
        let e2e_us = checkpoints.total_e2e_duration_us().unwrap_or(0);
        let is_spike = e2e_us > self.threshold_us;

        // 维护滑动历史窗口
        if self.history.len() >= self.pre_spike_capacity {
            self.history.pop_front();
        }
        self.history.push_back((frame_id, checkpoints));

        if is_spike {
            // 找出耗时最长的主因阶段
            let mut culprit_stage = StageId::Capture;
            let mut max_stage_dur_us = 0;

            for stage in StageId::ALL {
                if let Some(dur) = checkpoints.stage_duration_us(stage)
                    && dur > max_stage_dur_us
                {
                    max_stage_dur_us = dur;
                    culprit_stage = stage;
                }
            }

            // 导出包含前序帧和当前毛刺帧的完整 Chrome Trace JSON
            let frames_slice: Vec<(u64, FrameTimingCheckpoints)> =
                self.history.iter().copied().collect();
            let trace_json =
                ChromeTraceExporter::export_frames_trace(&frames_slice, self.session_id);

            let timestamp_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            let report = SpikeReport {
                spike_frame_id: frame_id,
                total_e2e_us: e2e_us,
                threshold_us: self.threshold_us,
                culprit_stage,
                culprit_duration_us: max_stage_dur_us,
                trace_json,
                timestamp_ms,
            };

            self.last_spike = Some(report.clone());
            Some(report)
        } else {
            None
        }
    }

    /// 获取最近一次触发的毛刺报告
    pub fn last_spike(&self) -> Option<&SpikeReport> {
        self.last_spike.as_ref()
    }
}

/// 瓶颈严重程度等级
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum BottleneckSeverity {
    None,
    Low,
    Medium,
    High,
    Critical,
}

impl fmt::Display for BottleneckSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BottleneckSeverity::None => write!(f, "NONE (流畅极佳)"),
            BottleneckSeverity::Low => write!(f, "LOW (轻微可优化)"),
            BottleneckSeverity::Medium => write!(f, "MEDIUM (中度瓶颈)"),
            BottleneckSeverity::High => write!(f, "HIGH (严重瓶颈)"),
            BottleneckSeverity::Critical => write!(f, "CRITICAL (致命阻塞)"),
        }
    }
}

/// 瓶颈诊断结果
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BottleneckDiagnosis {
    pub primary_stage: StageId,
    pub primary_stage_share_pct: f32,
    pub primary_stage_p99_us: u32,
    pub severity: BottleneckSeverity,
    pub root_cause: String,
    pub recommendation: String,
    pub secondary_bottlenecks: Vec<(StageId, f32)>,
    pub e2e_p50_ms: f32,
    pub e2e_p99_ms: f32,
}

/// 核心管线瓶颈归因与诊断分析器
pub struct BottleneckAnalyzer;

impl BottleneckAnalyzer {
    /// 基于 PipelineTelemetryReport 诊断核心性能瓶颈
    pub fn analyze(report: &PipelineTelemetryReport) -> BottleneckDiagnosis {
        let e2e_stats = &report.e2e_stats;
        let e2e_avg = e2e_stats.avg_us.max(1) as f32;
        let e2e_p50_ms = e2e_stats.p50_us as f32 / 1000.0;
        let e2e_p99_ms = e2e_stats.p99_us as f32 / 1000.0;

        let mut stage_shares: Vec<(StageId, f32, &StageLatencyStats)> = StageId::ALL
            .iter()
            .map(|&stage| {
                let st = &report.stage_stats[stage as usize];
                let share_pct = (st.avg_us as f32 / e2e_avg) * 100.0;
                (stage, share_pct, st)
            })
            .collect();

        // 按平均耗时占比从高到低排序
        stage_shares.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let (primary_stage, primary_share, primary_st) = stage_shares[0];

        // 识别次要瓶颈 (耗时占比 > 15% 的其他阶段)
        let secondary_bottlenecks: Vec<(StageId, f32)> = stage_shares
            .iter()
            .skip(1)
            .filter(|(_, share, _)| *share >= 15.0)
            .map(|(stage, share, _)| (*stage, *share))
            .collect();

        // 评估严重级别
        let severity = if e2e_p99_ms > 100.0 || primary_share > 60.0 {
            BottleneckSeverity::Critical
        } else if e2e_p99_ms > 50.0 || primary_share > 45.0 {
            BottleneckSeverity::High
        } else if e2e_p99_ms > 30.0 || primary_share > 35.0 {
            BottleneckSeverity::Medium
        } else if e2e_p99_ms > 20.0 || primary_share > 25.0 {
            BottleneckSeverity::Low
        } else {
            BottleneckSeverity::None
        };

        // 根因分析与调优建议规则引擎
        let (root_cause, recommendation) = match primary_stage {
            StageId::Capture => (
                format!(
                    "屏幕捕获阶段占比达到 {:.1}% (p99={:.2}ms)。OS 捕获回调或窗口表面格式转换耗时过长。",
                    primary_share, primary_st.p99_us as f32 / 1000.0
                ),
                "建议：检查 OS 屏幕录制权限，确保采用 ScreenCaptureKit 硬件加速抓取，避免逐帧像素格式软件转换。".to_string(),
            ),
            StageId::EncodeQueue => (
                format!(
                    "编码排队阶段占比达到 {:.1}% (p99={:.2}ms)。硬件编码器输入 FIFO 队列出现阻塞积压。",
                    primary_share, primary_st.p99_us as f32 / 1000.0
                ),
                "建议：降低 Host 端目标渲染帧率，或者提升编码器并发处理能力，防止画面帧在入队处排队。".to_string(),
            ),
            StageId::HardwareEncode => (
                format!(
                    "硬件编码阶段占比达到 {:.1}% (p99={:.2}ms)。GPU/ASIC 压缩吞吐能力已接近上限。",
                    primary_share, primary_st.p99_us as f32 / 1000.0
                ),
                "建议：将编码 Preset 调整为超低时延模式 (Realtime / LowLatency)，或适当降低推流分辨率与码率。".to_string(),
            ),
            StageId::PacketizeEgress => (
                format!(
                    "分片封包与发射阶段占比达到 {:.1}% (p99={:.2}ms)。UDP 发送套接字阻塞或分片内存深拷贝耗时偏高。",
                    primary_share, primary_st.p99_us as f32 / 1000.0
                ),
                "建议：启用零拷贝 Bytes 缓冲池，增大 SO_SNDBUF 套接字发送缓冲区，避免大帧发送锁争用。".to_string(),
            ),
            StageId::NetworkTransit => (
                format!(
                    "网络传输阶段占比达到 {:.1}% (p99={:.2}ms)。物理网络链路时延过高或 Wi-Fi 抖动严重。",
                    primary_share, primary_st.p99_us as f32 / 1000.0
                ),
                "建议：切换至 5GHz/有线以太网，开启 P2P 直连 Mesh 隧道，或调小视频码率以减少突发拥塞。".to_string(),
            ),
            StageId::IngressReassembly => (
                format!(
                    "接收与重组阶段占比达到 {:.1}% (p99={:.2}ms)。Client UDP Socket 读出延迟或分片重组等待。",
                    primary_share, primary_st.p99_us as f32 / 1000.0
                ),
                "建议：增大 Client 端 UDP 接收缓冲区 (SO_RCVBUF)，优化分片重组哈希表查询开销。".to_string(),
            ),
            StageId::JitterPacing => (
                format!(
                    "抖动缓冲与起搏阶段占比达到 {:.1}% (p99={:.2}ms)。Jitter Buffer 队列深度设置过大或起搏过于保守。",
                    primary_share, primary_st.p99_us as f32 / 1000.0
                ),
                "建议：调小 Jitter Buffer 目标驻留深度，开启激进晚帧丢弃以降低端到端显示时延。".to_string(),
            ),
            StageId::HardwareDecode => (
                format!(
                    "硬件解码阶段占比达到 {:.1}% (p99={:.2}ms)。Client 硬件解码器管线满载或回退至软解。",
                    primary_share, primary_st.p99_us as f32 / 1000.0
                ),
                "建议：确认开启 VideoToolbox / NVDEC 硬件硬解，避免 CPU 解码高分辨率 HEVC 视频流。".to_string(),
            ),
            StageId::RenderPresentation => (
                format!(
                    "渲染呈现阶段占比达到 {:.1}% (p99={:.2}ms)。Metal/GPUI 显示呈现回调或 VSync 等待耗时显著。",
                    primary_share, primary_st.p99_us as f32 / 1000.0
                ),
                "建议：检查显示器垂直同步 (VSync) 刷新率匹配情况，优化 Metal 纹理绑定与 Surface 交换链。".to_string(),
            ),
        };

        BottleneckDiagnosis {
            primary_stage,
            primary_stage_share_pct: primary_share,
            primary_stage_p99_us: primary_st.p99_us,
            severity,
            root_cause,
            recommendation,
            secondary_bottlenecks,
            e2e_p50_ms,
            e2e_p99_ms,
        }
    }

    /// 生成格式化 Markdown 瓶颈诊断分析报告
    pub fn format_diagnostic_report(diagnosis: &BottleneckDiagnosis) -> String {
        let mut out = String::with_capacity(1024);
        out.push_str("# Pipeline Bottleneck & Latency Diagnostic Report\n\n");
        out.push_str(&format!("- **Severity Level**: {}\n", diagnosis.severity));
        out.push_str(&format!(
            "- **End-to-End Latency**: p50 = {:.2}ms, p99 = {:.2}ms\n",
            diagnosis.e2e_p50_ms, diagnosis.e2e_p99_ms
        ));
        out.push_str(&format!(
            "- **Primary Bottleneck**: Stage [{}] (Accounting for {:.1}% of pipeline latency, p99={:.2}ms)\n\n",
            diagnosis.primary_stage.name(),
            diagnosis.primary_stage_share_pct,
            diagnosis.primary_stage_p99_us as f32 / 1000.0
        ));

        out.push_str("### Root Cause Analysis\n");
        out.push_str(&format!("{}\n\n", diagnosis.root_cause));

        out.push_str("### Actionable Optimization Recommendations\n");
        out.push_str(&format!("{}\n\n", diagnosis.recommendation));

        if !diagnosis.secondary_bottlenecks.is_empty() {
            out.push_str("### Secondary Latency Contributors (≥ 15%)\n");
            for (st, pct) in &diagnosis.secondary_bottlenecks {
                out.push_str(&format!("- **{}**: {:.1}% share\n", st.name(), pct));
            }
            out.push('\n');
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chrome_trace_export_format() {
        let mut cp = FrameTimingCheckpoints::new(1_700_000_000_000_000);
        cp.encode_queue_ts_us = 1_000;
        cp.encode_done_ts_us = 4_000;
        cp.packetize_ts_us = 4_500;
        cp.send_ts_us = 5_000;
        cp.recv_ts_us = 12_000;
        cp.jitter_enter_ts_us = 12_500;
        cp.jitter_exit_ts_us = 15_000;
        cp.decode_enter_ts_us = 15_500;
        cp.decode_done_ts_us = 18_000;
        cp.render_submit_ts_us = 18_500;
        cp.render_done_ts_us = 21_000;

        let json = ChromeTraceExporter::export_frames_trace(&[(1, cp), (2, cp)], 101);
        assert!(json.contains("\"traceEvents\""));
        assert!(json.contains("S1_Capture (F#1)"));
        assert!(json.contains("S4_NetworkTransit (F#1)"));
        assert!(json.contains("S8_RenderPresentation (F#2)"));
        assert!(json.contains("\"ph\": \"X\""));

        // Validate valid JSON parse
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert!(parsed["traceEvents"].is_array());
        assert_eq!(parsed["traceEvents"].as_array().unwrap().len(), 16);
    }

    #[test]
    fn test_trace_spike_trigger() {
        let mut trigger = TraceSpikeTrigger::new(77, 30_000, 20);
        assert_eq!(trigger.threshold_us(), 30_000);

        // Normal frame (20ms) -> No spike
        let mut normal_cp = FrameTimingCheckpoints::new(100_000);
        normal_cp.send_ts_us = 5_000;
        normal_cp.render_done_ts_us = 20_000;
        let res1 = trigger.record_frame(1, normal_cp);
        assert!(res1.is_none());

        // Spiking frame (60ms) -> Spike triggered!
        let mut spike_cp = FrameTimingCheckpoints::new(200_000);
        spike_cp.encode_queue_ts_us = 500;
        spike_cp.encode_done_ts_us = 2_000;
        spike_cp.send_ts_us = 3_000;
        spike_cp.recv_ts_us = 55_000; // 52ms network transit bottleneck
        spike_cp.render_done_ts_us = 60_000;

        let res2 = trigger.record_frame(2, spike_cp);
        assert!(res2.is_some());
        let report = res2.unwrap();
        assert_eq!(report.spike_frame_id, 2);
        assert_eq!(report.total_e2e_us, 60_000);
        assert_eq!(report.culprit_stage, StageId::NetworkTransit);
        assert!(report.trace_json.contains("S4_NetworkTransit (F#2)"));
        assert!(trigger.last_spike().is_some());
    }

    #[test]
    fn test_bottleneck_analyzer_diagnosis() {
        let mut report = PipelineTelemetryReport {
            session_id: 1,
            timestamp_ms: 1000,
            fps: 60.0,
            bitrate_kbps: 8000,
            stage_stats: [StageLatencyStats::default(); StageId::STAGE_COUNT],
            e2e_stats: StageLatencyStats {
                min_us: 15_000,
                p50_us: 25_000,
                p95_us: 45_000,
                p99_us: 65_000,
                max_us: 80_000,
                avg_us: 28_000,
                stddev_us: 8_000,
                sample_count: 100,
            },
            jitter_buffer_depth: 2,
            packets_lost: 0,
            late_frames_dropped: 0,
            queue_full_dropped: 0,
            corrupt_frames_dropped: 0,
        };

        // Network transit is dominating (18ms out of 28ms avg)
        report.stage_stats[StageId::NetworkTransit as usize] = StageLatencyStats {
            min_us: 10_000,
            p50_us: 16_000,
            p95_us: 35_000,
            p99_us: 50_000,
            max_us: 60_000,
            avg_us: 18_000,
            stddev_us: 5_000,
            sample_count: 100,
        };

        report.stage_stats[StageId::HardwareEncode as usize] = StageLatencyStats {
            min_us: 2_000,
            p50_us: 4_000,
            p95_us: 6_000,
            p99_us: 8_000,
            max_us: 10_000,
            avg_us: 5_000,
            stddev_us: 1_000,
            sample_count: 100,
        };

        let diag = BottleneckAnalyzer::analyze(&report);
        assert_eq!(diag.primary_stage, StageId::NetworkTransit);
        assert!(diag.primary_stage_share_pct > 50.0);
        assert_eq!(diag.severity, BottleneckSeverity::Critical);
        assert!(diag.root_cause.contains("网络传输阶段占比"));
        assert!(diag.recommendation.contains("5GHz/有线以太网"));

        let report_text = BottleneckAnalyzer::format_diagnostic_report(&diag);
        assert!(report_text.contains("# Pipeline Bottleneck & Latency Diagnostic Report"));
        assert!(report_text.contains("Primary Bottleneck"));
        assert!(report_text.contains("NetworkTransit"));
    }
}
