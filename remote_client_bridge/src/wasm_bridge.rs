#[cfg(feature = "wasm")]
use crate::{BridgeSessionState, BridgeTelemetry, RemoteBridgeClient, touch_mapper::TouchMode};
#[cfg(feature = "wasm")]
use protocol::TouchAction;
#[cfg(feature = "wasm")]
use std::sync::Arc;
#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::*;

#[cfg(feature = "wasm")]
#[wasm_bindgen]
pub struct RemotePlayWasmClient {
    inner: Arc<RemoteBridgeClient>,
}

#[cfg(feature = "wasm")]
#[wasm_bindgen]
impl RemotePlayWasmClient {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RemoteBridgeClient::new()),
        }
    }

    #[wasm_bindgen(js_name = connect)]
    pub fn connect(&self, device_id: String, endpoint: String) {
        self.inner.connect(device_id, endpoint);
    }

    #[wasm_bindgen(js_name = disconnect)]
    pub fn disconnect(&self) {
        self.inner.disconnect();
    }

    #[wasm_bindgen(js_name = sendTouch)]
    pub fn send_touch(
        &self,
        action_str: &str,
        pointer_id: u32,
        norm_x: f32,
        norm_y: f32,
        pressure: f32,
    ) {
        let action = match action_str.to_lowercase().as_str() {
            "down" | "touchstart" => TouchAction::Down,
            "move" | "touchmove" => TouchAction::Move,
            "up" | "touchend" => TouchAction::Up,
            _ => TouchAction::Cancel,
        };

        self.inner.handle_touch_input(action, pointer_id, norm_x, norm_y, pressure);
    }

    #[wasm_bindgen(js_name = sendVirtualKey)]
    pub fn send_virtual_key(&self, key_name: &str, pressed: bool) {
        self.inner.send_virtual_key(key_name, pressed);
    }

    #[wasm_bindgen(js_name = setTouchMode)]
    pub fn set_touch_mode(&self, mode_str: &str) {
        let mode = match mode_str.to_lowercase().as_str() {
            "trackpad" => TouchMode::VirtualTrackpad,
            "gamepad" => TouchMode::GamepadOverlay,
            _ => TouchMode::DirectTouch,
        };
        self.inner.set_touch_mode(mode);
    }

    #[wasm_bindgen(js_name = setScreenBounds)]
    pub fn set_screen_bounds(&self, width: u16, height: u16) {
        self.inner.set_screen_bounds(width, height);
    }

    #[wasm_bindgen(js_name = getTelemetryJson)]
    pub fn get_telemetry_json(&self) -> String {
        let telemetry = self.inner.telemetry_rx.borrow().clone();
        serde_json::to_string(&telemetry).unwrap_or_else(|_| "{}".to_string())
    }

    #[wasm_bindgen(js_name = getSessionState)]
    pub fn get_session_state(&self) -> String {
        match *self.inner.state_rx.borrow() {
            BridgeSessionState::Disconnected => "disconnected".to_string(),
            BridgeSessionState::Connecting => "connecting".to_string(),
            BridgeSessionState::Streaming => "streaming".to_string(),
            BridgeSessionState::Reconnecting => "reconnecting".to_string(),
            BridgeSessionState::Error => "error".to_string(),
        }
    }
}
