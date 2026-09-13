pub mod audio;
pub mod client_session;
pub mod clipboard_file_runtime;
pub mod clipboard_plane;
pub mod clipboard_provider;
pub mod clipboard_runtime;
pub mod clipboard_sync;
pub mod clock;
pub mod data_plane;
pub mod discovery;
pub mod file_transfer;
pub mod file_transfer_runtime;
pub mod jitter_buffer;
pub mod latest_frame;
pub mod media_plane;
pub mod mesh;
pub mod net;
pub mod p2p;
pub mod pairing_qr;
pub mod relay;
pub mod role;
pub mod scheduled_sender;
pub mod session_crypto;
pub mod stats;
pub mod telemetry;
pub mod timing;
pub mod trace;
pub mod traits;

// Re-export traits for convenience
pub use client_session::{
    AudioIngressEvent, ClientSessionEvent, ClientSessionReceiverConfig, EnvelopeIngress, HostStats,
    SharedHostStats, spawn_client_session_receiver,
};
pub use latest_frame::LatestFrameSlot;
pub use pairing_qr::{
    PairingQrPayload, QrMatrix, encode_pairing_qr, parse_pairing_qr, qr_matrix_from_payload,
};
pub use session_crypto::{
    SessionCrypto, SessionCryptoError, load_session_psk, mac_session_accept, mac_session_hello,
    require_session_auth, verify_session_mac,
};
pub use stats::Statistics;
pub use telemetry::{
    DropReason, PipelineTelemetryEngine, RollingBitrateCalculator, RollingFpsCalculator,
    RollingQuantileAggregator,
};
pub use timing::{
    ClientFrameTracker, ClockSynchronizer, HighPrecisionClock, HostFrameTracker,
    advance_client_stage, client_stage_offset_us, global_clock, quanta_now, quanta_now_us,
    stamp_client_stage,
};
pub use trace::{
    BottleneckAnalyzer, BottleneckDiagnosis, BottleneckSeverity, ChromeTraceDocument,
    ChromeTraceExporter, SpikeReport, TraceSpikeTrigger,
};
pub use traits::*;

pub fn init_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
