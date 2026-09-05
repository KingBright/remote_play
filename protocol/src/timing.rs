use serde::{Deserialize, Serialize};
use std::fmt;

pub const FRAME_TIMING_CHECKPOINTS_WIRE_LEN: usize = 52;
pub const HOST_TIMING_WIRE_LEN: usize = 24;

/// 跨端到端流媒体管线的 9 个阶段划分
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum StageId {
    /// S1: 屏幕/窗口捕获阶段 (OS 捕获回调触发至获得未压缩像素帧)
    Capture = 0,
    /// S2a: 编码入队与排队阶段 (像素帧进入编码器输入 FIFO 队列等待硬件调度)
    EncodeQueue = 1,
    /// S2b: 硬件编码阶段 (硬件 ASIC/GPU 开始压缩至 NALU 编码单元发射)
    HardwareEncode = 2,
    /// S3: 分片封包与发射阶段 (NALU 切片、RTP 封装与 UDP Socket 发送)
    PacketizeEgress = 3,
    /// S4: 网络传输阶段 (Host UDP Socket 发送至 Client UDP Socket 接收)
    NetworkTransit = 4,
    /// S5: 接收与重组阶段 (UDP 套接字读出、分片重组与完整帧组装)
    IngressReassembly = 5,
    /// S6: 抖动缓冲与起搏调度 (Jitter Buffer 入队驻留、平滑起搏与出队)
    JitterPacing = 6,
    /// S7: 硬件解码阶段 (解码入队、硬件解码执行至输出解压像素帧)
    HardwareDecode = 7,
    /// S8: 渲染呈现阶段 (GPU 渲染指令提交、Metal/Surface 呈现回调至 VSync 屏幕上屏)
    RenderPresentation = 8,
}

impl StageId {
    pub const ALL: [StageId; 9] = [
        StageId::Capture,
        StageId::EncodeQueue,
        StageId::HardwareEncode,
        StageId::PacketizeEgress,
        StageId::NetworkTransit,
        StageId::IngressReassembly,
        StageId::JitterPacing,
        StageId::HardwareDecode,
        StageId::RenderPresentation,
    ];

    pub const STAGE_COUNT: usize = 9;

    #[inline]
    pub const fn name(&self) -> &'static str {
        match self {
            StageId::Capture => "Capture",
            StageId::EncodeQueue => "EncodeQueue",
            StageId::HardwareEncode => "HardwareEncode",
            StageId::PacketizeEgress => "PacketizeEgress",
            StageId::NetworkTransit => "NetworkTransit",
            StageId::IngressReassembly => "IngressReassembly",
            StageId::JitterPacing => "JitterPacing",
            StageId::HardwareDecode => "HardwareDecode",
            StageId::RenderPresentation => "RenderPresentation",
        }
    }

    #[inline]
    pub const fn display_name(&self) -> &'static str {
        match self {
            StageId::Capture => "屏幕捕获",
            StageId::EncodeQueue => "编码排队",
            StageId::HardwareEncode => "硬件编码",
            StageId::PacketizeEgress => "分片发射",
            StageId::NetworkTransit => "网络传输",
            StageId::IngressReassembly => "接收重组",
            StageId::JitterPacing => "抖动起搏",
            StageId::HardwareDecode => "硬件解码",
            StageId::RenderPresentation => "渲染呈现",
        }
    }

    #[inline]
    pub const fn wire_id(&self) -> u8 {
        *self as u8
    }

    #[inline]
    pub fn from_wire_id(id: u8) -> Result<Self, TimingCodecError> {
        match id {
            0 => Ok(StageId::Capture),
            1 => Ok(StageId::EncodeQueue),
            2 => Ok(StageId::HardwareEncode),
            3 => Ok(StageId::PacketizeEgress),
            4 => Ok(StageId::NetworkTransit),
            5 => Ok(StageId::IngressReassembly),
            6 => Ok(StageId::JitterPacing),
            7 => Ok(StageId::HardwareDecode),
            8 => Ok(StageId::RenderPresentation),
            other => Err(TimingCodecError::InvalidStageId(other)),
        }
    }

    #[inline]
    pub const fn is_host(&self) -> bool {
        matches!(
            self,
            StageId::Capture
                | StageId::EncodeQueue
                | StageId::HardwareEncode
                | StageId::PacketizeEgress
        )
    }

    #[inline]
    pub const fn is_network(&self) -> bool {
        matches!(self, StageId::NetworkTransit)
    }

    #[inline]
    pub const fn is_client(&self) -> bool {
        matches!(
            self,
            StageId::IngressReassembly
                | StageId::JitterPacing
                | StageId::HardwareDecode
                | StageId::RenderPresentation
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimingCodecError {
    BufferTooShort {
        expected: usize,
        actual: usize,
    },
    InvalidStageId(u8),
    InvalidTimestampSequence {
        stage: StageId,
        prev_us: u32,
        next_us: u32,
    },
}

impl fmt::Display for TimingCodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TimingCodecError::BufferTooShort { expected, actual } => {
                write!(
                    f,
                    "Timing buffer too short: expected {expected}B, got {actual}B"
                )
            }
            TimingCodecError::InvalidStageId(id) => {
                write!(f, "Invalid StageId wire ID: {id}")
            }
            TimingCodecError::InvalidTimestampSequence {
                stage,
                prev_us,
                next_us,
            } => {
                write!(
                    f,
                    "Non-monotonic timestamp in stage {stage:?}: prev={prev_us}us, next={next_us}us"
                )
            }
        }
    }
}

impl std::error::Error for TimingCodecError {}

/// 全链路 12 处微秒时序检查点结构体 (52 字节定长二进制表示)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
pub struct FrameTimingCheckpoints {
    // ---- [Host 端打点 (S1 ~ S3)] ----
    /// S1: 画面捕获基准微秒时戳 (Unix Epoch 或 Host 同步单调时戳)
    pub capture_ts_us: u64,
    /// S1 -> S2: 进入硬件编码器队列微秒偏移
    pub encode_queue_ts_us: u32,
    /// S2: 硬件编码器压缩完成并输出 NALU 微秒偏移
    pub encode_done_ts_us: u32,
    /// S3: 完成 RTP/分片封包微秒偏移
    pub packetize_ts_us: u32,
    /// S3 -> S4: UDP Socket 发射完成微秒偏移
    pub send_ts_us: u32,

    // ---- [Client 端打点 (S4 ~ S8)] ----
    /// S4 -> S5: Client UDP Socket 接收微秒偏移 (已按时钟偏移对齐到 Host 坐标系)
    pub recv_ts_us: u32,
    /// S5 -> S6: 进入 Jitter Buffer 队列微秒偏移
    pub jitter_enter_ts_us: u32,
    /// S6: Jitter Buffer 出队/起搏完成微秒偏移
    pub jitter_exit_ts_us: u32,
    /// S6 -> S7: 提交至硬件解码器微秒偏移
    pub decode_enter_ts_us: u32,
    /// S7: 硬件解码完成并输出 CVPixelBuffer 微秒偏移
    pub decode_done_ts_us: u32,
    /// S7 -> S8: 提交 GPU Metal 渲染队列微秒偏移
    pub render_submit_ts_us: u32,
    /// S8: 渲染呈现/VSync 屏幕显示完成微秒偏移
    pub render_done_ts_us: u32,
}

impl FrameTimingCheckpoints {
    /// 创建新的帧时序检查点，以指定微秒时间戳为基准点
    #[inline]
    pub const fn new(capture_ts_us: u64) -> Self {
        Self {
            capture_ts_us,
            encode_queue_ts_us: 0,
            encode_done_ts_us: 0,
            packetize_ts_us: 0,
            send_ts_us: 0,
            recv_ts_us: 0,
            jitter_enter_ts_us: 0,
            jitter_exit_ts_us: 0,
            decode_enter_ts_us: 0,
            decode_done_ts_us: 0,
            render_submit_ts_us: 0,
            render_done_ts_us: 0,
        }
    }

    // -------------------------------------------------------------------------
    // 阶段耗时精准计算方法 (Microseconds Duration)
    // -------------------------------------------------------------------------

    /// S1: 捕获与准备耗时 (Capture Acquisition Duration)
    #[inline]
    pub fn capture_duration_us(&self) -> Option<u32> {
        if self.encode_queue_ts_us > 0 {
            Some(self.encode_queue_ts_us)
        } else {
            None
        }
    }

    /// S2a: 编码排队耗时 (Encoder Queue Wait Duration)
    #[inline]
    pub fn encode_queue_duration_us(&self) -> Option<u32> {
        if self.encode_done_ts_us > 0 && self.encode_queue_ts_us > 0 {
            Some(
                self.encode_done_ts_us
                    .saturating_sub(self.encode_queue_ts_us),
            )
        } else {
            None
        }
    }

    /// S2b: 硬件编码耗时 (Hardware Encode Compression Duration)
    #[inline]
    pub fn hardware_encode_duration_us(&self) -> Option<u32> {
        if self.encode_done_ts_us > 0 && self.encode_queue_ts_us > 0 {
            Some(
                self.encode_done_ts_us
                    .saturating_sub(self.encode_queue_ts_us),
            )
        } else if self.encode_done_ts_us > 0 {
            Some(self.encode_done_ts_us)
        } else {
            None
        }
    }

    /// S3: 分片封包与发射耗时 (Packetization & Network Egress Duration)
    #[inline]
    pub fn packetize_egress_duration_us(&self) -> Option<u32> {
        if self.send_ts_us > 0 && self.encode_done_ts_us > 0 {
            Some(self.send_ts_us.saturating_sub(self.encode_done_ts_us))
        } else {
            None
        }
    }

    /// S4: 网络传输耗时 (Network Transit Latency)
    #[inline]
    pub fn network_transit_duration_us(&self) -> Option<u32> {
        if self.recv_ts_us > 0 && self.send_ts_us > 0 {
            Some(self.recv_ts_us.saturating_sub(self.send_ts_us))
        } else {
            None
        }
    }

    /// S5: 接收与重组耗时 (Ingress & Reassembly Duration)
    #[inline]
    pub fn ingress_reassembly_duration_us(&self) -> Option<u32> {
        if self.jitter_enter_ts_us > 0 && self.recv_ts_us > 0 {
            Some(self.jitter_enter_ts_us.saturating_sub(self.recv_ts_us))
        } else {
            None
        }
    }

    /// S6: 抖动缓冲驻留与起搏耗时 (Jitter Buffer Residency & Pacing Delay)
    #[inline]
    pub fn jitter_pacing_duration_us(&self) -> Option<u32> {
        if self.jitter_exit_ts_us > 0 && self.jitter_enter_ts_us > 0 {
            Some(
                self.jitter_exit_ts_us
                    .saturating_sub(self.jitter_enter_ts_us),
            )
        } else {
            None
        }
    }

    /// S7: 硬件解码耗时 (Hardware Decompression Duration)
    #[inline]
    pub fn hardware_decode_duration_us(&self) -> Option<u32> {
        if self.decode_done_ts_us > 0 && self.decode_enter_ts_us > 0 {
            Some(
                self.decode_done_ts_us
                    .saturating_sub(self.decode_enter_ts_us),
            )
        } else {
            None
        }
    }

    /// S8: 渲染与呈现耗时 (Render Submission to Display VSync Presentation)
    #[inline]
    pub fn render_presentation_duration_us(&self) -> Option<u32> {
        if self.render_done_ts_us > 0 && self.render_submit_ts_us > 0 {
            Some(
                self.render_done_ts_us
                    .saturating_sub(self.render_submit_ts_us),
            )
        } else {
            None
        }
    }

    /// 查询任意指定阶段的耗时（微秒）
    pub fn stage_duration_us(&self, stage: StageId) -> Option<u32> {
        match stage {
            StageId::Capture => self.capture_duration_us(),
            StageId::EncodeQueue => self.encode_queue_duration_us(),
            StageId::HardwareEncode => self.hardware_encode_duration_us(),
            StageId::PacketizeEgress => self.packetize_egress_duration_us(),
            StageId::NetworkTransit => self.network_transit_duration_us(),
            StageId::IngressReassembly => self.ingress_reassembly_duration_us(),
            StageId::JitterPacing => self.jitter_pacing_duration_us(),
            StageId::HardwareDecode => self.hardware_decode_duration_us(),
            StageId::RenderPresentation => self.render_presentation_duration_us(),
        }
    }

    // -------------------------------------------------------------------------
    // 汇总耗时与端到端耗时
    // -------------------------------------------------------------------------

    /// Host 端总耗时 (S1 ~ S3)
    #[inline]
    pub fn host_pipeline_duration_us(&self) -> Option<u32> {
        if self.send_ts_us > 0 {
            Some(self.send_ts_us)
        } else {
            None
        }
    }

    /// Client 端总耗时 (S5 ~ S8)
    #[inline]
    pub fn client_pipeline_duration_us(&self) -> Option<u32> {
        if self.render_done_ts_us > 0 && self.recv_ts_us > 0 {
            Some(self.render_done_ts_us.saturating_sub(self.recv_ts_us))
        } else {
            None
        }
    }

    /// 端到端总时延 (End-to-End Latency: Capture -> Screen Render)
    #[inline]
    pub fn total_e2e_duration_us(&self) -> Option<u32> {
        if self.render_done_ts_us > 0 {
            Some(self.render_done_ts_us)
        } else {
            None
        }
    }

    // -------------------------------------------------------------------------
    // 定长二进制零拷贝编解码 (Fixed-Size Binary Serialization)
    // -------------------------------------------------------------------------

    /// 完整 52 字节网络序列化（大端序）
    pub fn to_wire_bytes(&self) -> [u8; FRAME_TIMING_CHECKPOINTS_WIRE_LEN] {
        let mut bytes = [0u8; FRAME_TIMING_CHECKPOINTS_WIRE_LEN];
        bytes[0..8].copy_from_slice(&self.capture_ts_us.to_be_bytes());
        bytes[8..12].copy_from_slice(&self.encode_queue_ts_us.to_be_bytes());
        bytes[12..16].copy_from_slice(&self.encode_done_ts_us.to_be_bytes());
        bytes[16..20].copy_from_slice(&self.packetize_ts_us.to_be_bytes());
        bytes[20..24].copy_from_slice(&self.send_ts_us.to_be_bytes());
        bytes[24..28].copy_from_slice(&self.recv_ts_us.to_be_bytes());
        bytes[28..32].copy_from_slice(&self.jitter_enter_ts_us.to_be_bytes());
        bytes[32..36].copy_from_slice(&self.jitter_exit_ts_us.to_be_bytes());
        bytes[36..40].copy_from_slice(&self.decode_enter_ts_us.to_be_bytes());
        bytes[40..44].copy_from_slice(&self.decode_done_ts_us.to_be_bytes());
        bytes[44..48].copy_from_slice(&self.render_submit_ts_us.to_be_bytes());
        bytes[48..52].copy_from_slice(&self.render_done_ts_us.to_be_bytes());
        bytes
    }

    /// 完整 52 字节网络反序列化（大端序）
    pub fn from_wire_bytes(bytes: &[u8]) -> Result<Self, TimingCodecError> {
        if bytes.len() < FRAME_TIMING_CHECKPOINTS_WIRE_LEN {
            return Err(TimingCodecError::BufferTooShort {
                expected: FRAME_TIMING_CHECKPOINTS_WIRE_LEN,
                actual: bytes.len(),
            });
        }
        Ok(Self {
            capture_ts_us: u64::from_be_bytes(bytes[0..8].try_into().unwrap()),
            encode_queue_ts_us: u32::from_be_bytes(bytes[8..12].try_into().unwrap()),
            encode_done_ts_us: u32::from_be_bytes(bytes[12..16].try_into().unwrap()),
            packetize_ts_us: u32::from_be_bytes(bytes[16..20].try_into().unwrap()),
            send_ts_us: u32::from_be_bytes(bytes[20..24].try_into().unwrap()),
            recv_ts_us: u32::from_be_bytes(bytes[24..28].try_into().unwrap()),
            jitter_enter_ts_us: u32::from_be_bytes(bytes[28..32].try_into().unwrap()),
            jitter_exit_ts_us: u32::from_be_bytes(bytes[32..36].try_into().unwrap()),
            decode_enter_ts_us: u32::from_be_bytes(bytes[36..40].try_into().unwrap()),
            decode_done_ts_us: u32::from_be_bytes(bytes[40..44].try_into().unwrap()),
            render_submit_ts_us: u32::from_be_bytes(bytes[44..48].try_into().unwrap()),
            render_done_ts_us: u32::from_be_bytes(bytes[48..52].try_into().unwrap()),
        })
    }

    /// Host 侧 24 字节载荷序列化（仅传输 Host 侧 5 个字段，削减 54% 传输负载）
    pub fn to_host_wire_bytes(&self) -> [u8; HOST_TIMING_WIRE_LEN] {
        let mut bytes = [0u8; HOST_TIMING_WIRE_LEN];
        bytes[0..8].copy_from_slice(&self.capture_ts_us.to_be_bytes());
        bytes[8..12].copy_from_slice(&self.encode_queue_ts_us.to_be_bytes());
        bytes[12..16].copy_from_slice(&self.encode_done_ts_us.to_be_bytes());
        bytes[16..20].copy_from_slice(&self.packetize_ts_us.to_be_bytes());
        bytes[20..24].copy_from_slice(&self.send_ts_us.to_be_bytes());
        bytes
    }

    /// Client 侧接收 Host 24 字节载荷并还原初始化
    pub fn from_host_wire_bytes(bytes: &[u8]) -> Result<Self, TimingCodecError> {
        if bytes.len() < HOST_TIMING_WIRE_LEN {
            return Err(TimingCodecError::BufferTooShort {
                expected: HOST_TIMING_WIRE_LEN,
                actual: bytes.len(),
            });
        }
        Ok(Self {
            capture_ts_us: u64::from_be_bytes(bytes[0..8].try_into().unwrap()),
            encode_queue_ts_us: u32::from_be_bytes(bytes[8..12].try_into().unwrap()),
            encode_done_ts_us: u32::from_be_bytes(bytes[12..16].try_into().unwrap()),
            packetize_ts_us: u32::from_be_bytes(bytes[16..20].try_into().unwrap()),
            send_ts_us: u32::from_be_bytes(bytes[20..24].try_into().unwrap()),
            recv_ts_us: 0,
            jitter_enter_ts_us: 0,
            jitter_exit_ts_us: 0,
            decode_enter_ts_us: 0,
            decode_done_ts_us: 0,
            render_submit_ts_us: 0,
            render_done_ts_us: 0,
        })
    }

    /// 将一帧的 8 阶段打点导出为一组 TraceSpan 事件 (Chrome Trace 格式)
    pub fn to_trace_spans(&self, frame_id: u64, session_id: u32) -> Vec<TraceSpan> {
        let mut spans = Vec::with_capacity(StageId::STAGE_COUNT);
        let base_ts = self.capture_ts_us;

        let mut add_span =
            |name: &'static str, cat: &'static str, start_offset: u32, end_offset: u32| {
                if end_offset > start_offset {
                    spans.push(TraceSpan {
                        name: format!("{name} (F#{frame_id})"),
                        cat: cat.to_string(),
                        ph: 'X',
                        ts_us: base_ts + start_offset as u64,
                        dur_us: (end_offset - start_offset) as u64,
                        pid: session_id,
                        tid: 1,
                    });
                }
            };

        // S1: Capture (0 -> encode_queue)
        add_span("S1_Capture", "host.video", 0, self.encode_queue_ts_us);
        // S2: Hardware Encode (encode_queue -> encode_done)
        add_span(
            "S2_HardwareEncode",
            "host.video",
            self.encode_queue_ts_us,
            self.encode_done_ts_us,
        );
        // S3: Packetize Egress (encode_done -> send)
        add_span(
            "S3_PacketizeEgress",
            "host.net",
            self.encode_done_ts_us,
            self.send_ts_us,
        );
        // S4: Network Transit (send -> recv)
        add_span(
            "S4_NetworkTransit",
            "network",
            self.send_ts_us,
            self.recv_ts_us,
        );
        // S5: Ingress Reassembly (recv -> jitter_enter)
        add_span(
            "S5_IngressReassembly",
            "client.net",
            self.recv_ts_us,
            self.jitter_enter_ts_us,
        );
        // S6: Jitter Pacing (jitter_enter -> jitter_exit)
        add_span(
            "S6_JitterPacing",
            "client.video",
            self.jitter_enter_ts_us,
            self.jitter_exit_ts_us,
        );
        // S7: Hardware Decode (decode_enter -> decode_done)
        add_span(
            "S7_HardwareDecode",
            "client.video",
            self.decode_enter_ts_us,
            self.decode_done_ts_us,
        );
        // S8: Render Presentation (render_submit -> render_done)
        add_span(
            "S8_RenderPresentation",
            "client.gpu",
            self.render_submit_ts_us,
            self.render_done_ts_us,
        );

        spans
    }
}

/// 单个阶段的微秒统计分位数
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StageLatencyStats {
    pub min_us: u32,
    pub p50_us: u32,
    pub p95_us: u32,
    pub p99_us: u32,
    pub max_us: u32,
    pub avg_us: u32,
    pub stddev_us: u32,
    pub sample_count: u32,
}

/// 诊断事件跨度，兼容 Chrome Trace Event Format (chrome://tracing / Perfetto)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceSpan {
    pub name: String,
    pub cat: String,
    pub ph: char, // 'X' for Complete Event, 'i' for Instant
    #[serde(rename = "ts")]
    pub ts_us: u64,
    #[serde(rename = "dur")]
    pub dur_us: u64,
    pub pid: u32,
    pub tid: u32,
}

/// 8 阶段端到端管线实时遥测报告
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PipelineTelemetryReport {
    pub session_id: u32,
    pub timestamp_ms: u64,
    pub fps: f32,
    pub bitrate_kbps: u32,
    /// 9 个阶段分别对应的分位数统计
    pub stage_stats: [StageLatencyStats; StageId::STAGE_COUNT],
    /// 总端到端时延分布
    pub e2e_stats: StageLatencyStats,
    /// Jitter Buffer 当前队列深度
    pub jitter_buffer_depth: usize,
    /// 丢包与丢帧健康指标
    pub packets_lost: u64,
    pub late_frames_dropped: u64,
    pub queue_full_dropped: u64,
    pub corrupt_frames_dropped: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stage_id_wire_roundtrip() {
        for stage in StageId::ALL {
            let wire_id = stage.wire_id();
            let recovered = StageId::from_wire_id(wire_id).unwrap();
            assert_eq!(stage, recovered);
            assert!(!stage.name().is_empty());
            assert!(!stage.display_name().is_empty());
        }
        assert!(StageId::from_wire_id(99).is_err());
    }

    #[test]
    fn test_checkpoints_wire_roundtrip() {
        let mut checkpoints = FrameTimingCheckpoints::new(1_700_000_000_000_000);
        checkpoints.encode_queue_ts_us = 1_200;
        checkpoints.encode_done_ts_us = 4_500;
        checkpoints.packetize_ts_us = 4_800;
        checkpoints.send_ts_us = 5_000;
        checkpoints.recv_ts_us = 12_000;
        checkpoints.jitter_enter_ts_us = 12_200;
        checkpoints.jitter_exit_ts_us = 15_000;
        checkpoints.decode_enter_ts_us = 15_300;
        checkpoints.decode_done_ts_us = 18_500;
        checkpoints.render_submit_ts_us = 18_800;
        checkpoints.render_done_ts_us = 22_000;

        let full_wire = checkpoints.to_wire_bytes();
        assert_eq!(full_wire.len(), FRAME_TIMING_CHECKPOINTS_WIRE_LEN);
        let recovered = FrameTimingCheckpoints::from_wire_bytes(&full_wire).unwrap();
        assert_eq!(checkpoints, recovered);

        let host_wire = checkpoints.to_host_wire_bytes();
        assert_eq!(host_wire.len(), HOST_TIMING_WIRE_LEN);
        let host_recovered = FrameTimingCheckpoints::from_host_wire_bytes(&host_wire).unwrap();
        assert_eq!(host_recovered.capture_ts_us, checkpoints.capture_ts_us);
        assert_eq!(host_recovered.send_ts_us, checkpoints.send_ts_us);
        assert_eq!(host_recovered.recv_ts_us, 0);
    }

    #[test]
    fn test_stage_duration_calculation() {
        let mut checkpoints = FrameTimingCheckpoints::new(1_000_000);
        checkpoints.encode_queue_ts_us = 1_000;
        checkpoints.encode_done_ts_us = 4_000;
        checkpoints.packetize_ts_us = 4_500;
        checkpoints.send_ts_us = 5_000;
        checkpoints.recv_ts_us = 12_000;
        checkpoints.jitter_enter_ts_us = 12_500;
        checkpoints.jitter_exit_ts_us = 16_000;
        checkpoints.decode_enter_ts_us = 16_500;
        checkpoints.decode_done_ts_us = 20_000;
        checkpoints.render_submit_ts_us = 20_500;
        checkpoints.render_done_ts_us = 24_000;

        assert_eq!(checkpoints.capture_duration_us(), Some(1_000));
        assert_eq!(checkpoints.encode_queue_duration_us(), Some(3_000));
        assert_eq!(checkpoints.hardware_encode_duration_us(), Some(3_000));
        assert_eq!(checkpoints.packetize_egress_duration_us(), Some(1_000));
        assert_eq!(checkpoints.network_transit_duration_us(), Some(7_000));
        assert_eq!(checkpoints.ingress_reassembly_duration_us(), Some(500));
        assert_eq!(checkpoints.jitter_pacing_duration_us(), Some(3_500));
        assert_eq!(checkpoints.hardware_decode_duration_us(), Some(3_500));
        assert_eq!(checkpoints.render_presentation_duration_us(), Some(3_500));

        assert_eq!(checkpoints.host_pipeline_duration_us(), Some(5_000));
        assert_eq!(checkpoints.client_pipeline_duration_us(), Some(12_000));
        assert_eq!(checkpoints.total_e2e_duration_us(), Some(24_000));

        for stage in StageId::ALL {
            assert!(checkpoints.stage_duration_us(stage).is_some());
        }
    }

    #[test]
    fn test_trace_spans_generation() {
        let mut checkpoints = FrameTimingCheckpoints::new(100_000);
        checkpoints.encode_queue_ts_us = 500;
        checkpoints.encode_done_ts_us = 2_000;
        checkpoints.send_ts_us = 3_000;
        checkpoints.recv_ts_us = 8_000;
        checkpoints.jitter_enter_ts_us = 8_500;
        checkpoints.jitter_exit_ts_us = 11_000;
        checkpoints.decode_enter_ts_us = 11_500;
        checkpoints.decode_done_ts_us = 14_000;
        checkpoints.render_submit_ts_us = 14_500;
        checkpoints.render_done_ts_us = 17_000;

        let spans = checkpoints.to_trace_spans(42, 101);
        assert_eq!(spans.len(), 8);
        assert_eq!(spans[0].name, "S1_Capture (F#42)");
        assert_eq!(spans[0].dur_us, 500);
        assert_eq!(spans[0].ts_us, 100_000);
        assert_eq!(spans[0].ph, 'X');
        assert_eq!(spans[0].pid, 101);
    }
}
