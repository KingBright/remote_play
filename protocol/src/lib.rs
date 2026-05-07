use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadType {
    VideoH265 = 96,
    AudioOpus = 97,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RtpHeader {
    pub version: u8,
    pub payload_type: u8,
    pub sequence_number: u16,
    pub timestamp: u32,
    pub ssrc: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RtpPacket {
    pub header: RtpHeader,
    pub payload: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ControlMessage {
    HandshakeReq,
    HandshakeAck,
    /// Sent at 1000Hz from client to host
    Input(InputEvent),
    /// Sent from host to client for Force Feedback (Future)
    ForceFeedback,
    /// Sent at 1Hz from host to client with host-side telemetry
    HostTelemetry {
        fps: f32,
        encode_latency_ms: f32,
        jitter_ms: f32,
        bitrate_kbps: u32,
    },
    /// Start capturing and streaming
    StartStream {
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
        session_id: u32,
    },
    /// Stop streaming and return to standby
    StopStream,
    /// Keep-alive ping from client to host
    Heartbeat,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum InputEvent {
    KeyDown(u32), // Scancode
    KeyUp(u32),
    MouseMove { dx: i32, dy: i32 },
    MouseDown(u8), // Button ID
    MouseUp(u8),
}

impl RtpPacket {
    pub fn encode(&self) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(self)
    }

    pub fn decode(data: &[u8]) -> Result<Self, bincode::Error> {
        bincode::deserialize(data)
    }
}

impl ControlMessage {
    pub fn encode(&self) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(self)
    }

    pub fn decode(data: &[u8]) -> Result<Self, bincode::Error> {
        bincode::deserialize(data)
    }
}
