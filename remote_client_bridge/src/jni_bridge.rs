#[cfg(feature = "android")]
use crate::{RemoteBridgeClient, touch_mapper::TouchMode};
#[cfg(feature = "android")]
use jni::JNIEnv;
#[cfg(feature = "android")]
use jni::objects::{JClass, JString};
#[cfg(feature = "android")]
use jni::sys::{jboolean, jbyteArray, jfloat, jint, jstring};
#[cfg(feature = "android")]
use protocol::TouchAction;
#[cfg(feature = "android")]
use std::sync::{Arc, OnceLock};

#[cfg(feature = "android")]
static GLOBAL_CLIENT: OnceLock<Arc<RemoteBridgeClient>> = OnceLock::new();

#[cfg(feature = "android")]
fn client() -> Arc<RemoteBridgeClient> {
    GLOBAL_CLIENT
        .get_or_init(|| Arc::new(RemoteBridgeClient::new()))
        .clone()
}

#[cfg(feature = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativeInit(
    _env: JNIEnv,
    _class: JClass,
) -> jboolean {
    let _ = client();
    1
}

#[cfg(feature = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativeConnect(
    mut env: JNIEnv,
    _class: JClass,
    device_id: JString,
    endpoint: JString,
) {
    let d_id: String = match env.get_string(&device_id) {
        Ok(s) => s.into(),
        Err(_) => return,
    };
    let ep: String = match env.get_string(&endpoint) {
        Ok(s) => s.into(),
        Err(_) => return,
    };
    client().connect(d_id, ep);
}

#[cfg(feature = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativeDisconnect(
    _env: JNIEnv,
    _class: JClass,
) {
    client().disconnect();
}

#[cfg(feature = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativeSendTouch(
    _env: JNIEnv,
    _class: JClass,
    action_code: jint,
    pointer_id: jint,
    norm_x: jfloat,
    norm_y: jfloat,
    pressure: jfloat,
) {
    let action = match action_code {
        0 => TouchAction::Down,
        1 => TouchAction::Move,
        2 => TouchAction::Up,
        _ => TouchAction::Cancel,
    };
    client().handle_touch_input(action, pointer_id as u32, norm_x, norm_y, pressure);
}

#[cfg(feature = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativeSendVirtualKey(
    mut env: JNIEnv,
    _class: JClass,
    key_name: JString,
    pressed: jboolean,
) {
    let key: String = match env.get_string(&key_name) {
        Ok(s) => s.into(),
        Err(_) => return,
    };
    client().send_virtual_key(&key, pressed != 0);
}

#[cfg(feature = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativeSetTouchMode(
    _env: JNIEnv,
    _class: JClass,
    mode_code: jint,
) {
    let mode = match mode_code {
        1 => TouchMode::VirtualTrackpad,
        2 => TouchMode::GamepadOverlay,
        _ => TouchMode::DirectTouch,
    };
    client().set_touch_mode(mode);
}

#[cfg(feature = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativeSetScreenBounds(
    _env: JNIEnv,
    _class: JClass,
    width: jint,
    height: jint,
) {
    client().set_screen_bounds(width.max(1) as u16, height.max(1) as u16);
}

#[cfg(feature = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativeGetTelemetryJson(
    env: JNIEnv,
    _class: JClass,
) -> jstring {
    let client = client();
    client.maybe_reconnect();
    client.refresh_telemetry_from_stats();
    let telemetry = client.telemetry_rx.borrow().clone();
    let json = serde_json::to_string(&telemetry).unwrap_or_else(|_| "{}".to_string());
    match env.new_string(json) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

#[cfg(feature = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativePollVideoFrame(
    env: JNIEnv,
    _class: JClass,
) -> jbyteArray {
    match client().poll_video_nalu() {
        Some(nalu) => {
            let mut packed = Vec::with_capacity(9 + nalu.data.len());
            packed.push(u8::from(nalu.keyframe));
            packed.extend_from_slice(&nalu.pts_us.to_le_bytes());
            packed.extend_from_slice(&nalu.data);
            match env.byte_array_from_slice(&packed) {
                Ok(array) => array.into_raw(),
                Err(_) => std::ptr::null_mut(),
            }
        }
        None => std::ptr::null_mut(),
    }
}

#[cfg(feature = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativeRequestKeyframe(
    _env: JNIEnv,
    _class: JClass,
) {
    client().request_keyframe();
}

#[cfg(feature = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativeGetSessionState(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    match *client().state_rx.borrow() {
        crate::BridgeSessionState::Disconnected => 0,
        crate::BridgeSessionState::Connecting => 1,
        crate::BridgeSessionState::Streaming => 2,
        crate::BridgeSessionState::Reconnecting => 3,
        crate::BridgeSessionState::Error => 4,
    }
}

#[cfg(feature = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativePollAudioPacket(
    env: JNIEnv,
    _class: JClass,
) -> jbyteArray {
    match client().poll_audio_packet() {
        Some(packet) => {
            let mut packed = Vec::with_capacity(6 + packet.data.len());
            packed.extend_from_slice(&packet.sample_rate_hz.to_le_bytes());
            packed.extend_from_slice(&packet.channels.to_le_bytes());
            packed.extend_from_slice(&packet.data);
            match env.byte_array_from_slice(&packed) {
                Ok(array) => array.into_raw(),
                Err(_) => std::ptr::null_mut(),
            }
        }
        None => std::ptr::null_mut(),
    }
}

#[cfg(feature = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativeGetDevicesJson(
    env: JNIEnv,
    _class: JClass,
) -> jstring {
    match env.new_string(client().devices_json()) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

#[cfg(feature = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativeJoinPairingPayload(
    mut env: JNIEnv,
    _class: JClass,
    payload: JString,
) -> jstring {
    let raw: String = match env.get_string(&payload) {
        Ok(s) => s.into(),
        Err(_) => return std::ptr::null_mut(),
    };
    let result = match client().join_pairing_payload(&raw) {
        Ok(message) => message,
        Err(err) => format!("error:{err}"),
    };
    match env.new_string(result) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}
