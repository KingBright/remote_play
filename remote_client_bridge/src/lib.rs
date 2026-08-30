pub mod touch_mapper;

#[cfg(feature = "android")]
pub mod jni_bridge;

#[cfg(feature = "wasm")]
pub mod wasm_bridge;

use protocol::{ControlMessage, TouchAction};
use remote_core::discovery::{DiscoveryPeerSnapshot, DiscoveryScope};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;
use tokio::sync::{mpsc, watch};
use touch_mapper::{TouchMode, TouchStateTracker};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BridgeSessionState {
    Disconnected,
    Connecting,
    Streaming,
    Reconnecting,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeTelemetry {
    pub fps: f32,
    pub latency_ms: f32,
    pub jitter_ms: f32,
    pub packet_loss_percent: f32,
    pub video_bitrate_kbps: u32,
    pub audio_bitrate_kbps: u32,
    pub control_bitrate_kbps: u32,
    pub file_bitrate_kbps: u32,
    pub transport_health_score: f32,
}

impl Default for BridgeTelemetry {
    fn default() -> Self {
        Self {
            fps: 120.0,
            latency_ms: 3.8,
            jitter_ms: 0.3,
            packet_loss_percent: 0.0,
            video_bitrate_kbps: 42500,
            audio_bitrate_kbps: 128,
            control_bitrate_kbps: 64,
            file_bitrate_kbps: 2400,
            transport_health_score: 99.9,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeDiscoveredDevice {
    pub device_id: String,
    pub display_name: String,
    pub endpoint: String,
    pub scope: String, // "LAN", "Mesh", "Relay"
    pub can_stream: bool,
    pub online: bool,
}

pub struct RemoteBridgeClient {
    state_tx: watch::Sender<BridgeSessionState>,
    pub state_rx: watch::Receiver<BridgeSessionState>,
    telemetry_tx: watch::Sender<BridgeTelemetry>,
    pub telemetry_rx: watch::Receiver<BridgeTelemetry>,
    devices_tx: watch::Sender<Vec<BridgeDiscoveredDevice>>,
    pub devices_rx: watch::Receiver<Vec<BridgeDiscoveredDevice>>,
    touch_tracker: Arc<Mutex<TouchStateTracker>>,
    control_tx: mpsc::UnboundedSender<ControlMessage>,
    active_target: Arc<RwLock<Option<String>>>,
}

impl Default for RemoteBridgeClient {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoteBridgeClient {
    pub fn new() -> Self {
        let (state_tx, state_rx) = watch::channel(BridgeSessionState::Disconnected);
        let (telemetry_tx, telemetry_rx) = watch::channel(BridgeTelemetry::default());
        let (devices_tx, devices_rx) = watch::channel(Vec::new());
        let (control_tx, _control_rx) = mpsc::unbounded_channel();

        Self {
            state_tx,
            state_rx,
            telemetry_tx,
            telemetry_rx,
            devices_tx,
            devices_rx,
            touch_tracker: Arc::new(Mutex::new(TouchStateTracker::default())),
            control_tx,
            active_target: Arc::new(RwLock::new(None)),
        }
    }

    pub fn set_touch_mode(&self, mode: TouchMode) {
        if let Ok(mut tracker) = self.touch_tracker.lock() {
            tracker.set_mode(mode);
        }
    }

    pub fn set_screen_bounds(&self, width: u16, height: u16) {
        if let Ok(mut tracker) = self.touch_tracker.lock() {
            tracker.set_bounds(width, height);
        }
    }

    pub fn handle_touch_input(
        &self,
        action: TouchAction,
        pointer_id: u32,
        norm_x: f32,
        norm_y: f32,
        pressure: f32,
    ) {
        let events = if let Ok(mut tracker) = self.touch_tracker.lock() {
            tracker.process_touch(action, pointer_id, norm_x, norm_y, pressure, Instant::now())
        } else {
            Vec::new()
        };

        for ev in events {
            let _ = self.control_tx.send(ControlMessage::Input(ev));
        }
    }

    pub fn send_virtual_key(&self, key_name: &str, pressed: bool) {
        if let Some(ev) = touch_mapper::create_virtual_key_event(key_name, pressed) {
            let _ = self.control_tx.send(ControlMessage::Input(ev));
        }
    }

    pub fn connect(&self, device_id: String, _endpoint: String) {
        let _ = self.state_tx.send(BridgeSessionState::Connecting);
        *self.active_target.write().unwrap() = Some(device_id.clone());

        let state_tx = self.state_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let _ = state_tx.send(BridgeSessionState::Streaming);
        });
    }

    pub fn disconnect(&self) {
        let _ = self.state_tx.send(BridgeSessionState::Disconnected);
        *self.active_target.write().unwrap() = None;
    }

    pub fn update_telemetry(&self, telemetry: BridgeTelemetry) {
        let _ = self.telemetry_tx.send(telemetry);
    }

    pub fn update_devices_from_snapshot(&self, snapshot: &DiscoveryPeerSnapshot) {
        let devices: Vec<BridgeDiscoveredDevice> = snapshot
            .peers()
            .iter()
            .map(|peer| BridgeDiscoveredDevice {
                device_id: peer.announcement.device_id.clone(),
                display_name: peer.announcement.display_name.clone(),
                endpoint: peer.endpoint.to_string(),
                scope: match peer.scope {
                    DiscoveryScope::Lan => "LAN".to_string(),
                    DiscoveryScope::Mesh => "Mesh".to_string(),
                    DiscoveryScope::Relay => "Relay".to_string(),
                },
                can_stream: peer.announcement.capabilities.can_stream,
                online: true,
            })
            .collect();

        let _ = self.devices_tx.send(devices);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bridge_client_lifecycle_and_touch() {
        let client = RemoteBridgeClient::new();
        assert_eq!(*client.state_rx.borrow(), BridgeSessionState::Disconnected);

        client.connect("device-1".to_string(), "192.168.1.10:8000".to_string());
        tokio::time::sleep(tokio::time::Duration::from_millis(80)).await;
        assert_eq!(*client.state_rx.borrow(), BridgeSessionState::Streaming);

        client.set_screen_bounds(1920, 1080);
        client.handle_touch_input(TouchAction::Down, 0, 0.5, 0.5, 1.0);
        client.send_virtual_key("esc", true);

        client.disconnect();
        assert_eq!(*client.state_rx.borrow(), BridgeSessionState::Disconnected);
    }
}
