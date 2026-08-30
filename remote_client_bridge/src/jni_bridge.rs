#[cfg(feature = "android")]
use crate::{BridgeTelemetry, RemoteBridgeClient, touch_mapper::TouchMode};
#[cfg(feature = "android")]
use jni::JNIEnv;
#[cfg(feature = "android")]
use jni::objects::{JClass, JString};
#[cfg(feature = "android")]
use jni::sys::{jboolean, jfloat, jint, jlong, jstring};
#[cfg(feature = "android")]
use protocol::TouchAction;
#[cfg(feature = "android")]
use std::sync::Arc;

#[cfg(feature = "android")]
static mut GLOBAL_CLIENT: Option<Arc<RemoteBridgeClient>> = None;

#[cfg(feature = "android")]
#[no_mangle]
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativeInit(
    _env: JNIEnv,
    _class: JClass,
) -> jboolean {
    unsafe {
        if GLOBAL_CLIENT.is_none() {
            GLOBAL_CLIENT = Some(Arc::new(RemoteBridgeClient::new()));
        }
    }
    1
}

#[cfg(feature = "android")]
#[no_mangle]
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

    unsafe {
        if let Some(ref client) = GLOBAL_CLIENT {
            client.connect(d_id, ep);
        }
    }
}

#[cfg(feature = "android")]
#[no_mangle]
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativeDisconnect(
    _env: JNIEnv,
    _class: JClass,
) {
    unsafe {
        if let Some(ref client) = GLOBAL_CLIENT {
            client.disconnect();
        }
    }
}

#[cfg(feature = "android")]
#[no_mangle]
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

    unsafe {
        if let Some(ref client) = GLOBAL_CLIENT {
            client.handle_touch_input(action, pointer_id as u32, norm_x, norm_y, pressure);
        }
    }
}

#[cfg(feature = "android")]
#[no_mangle]
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

    unsafe {
        if let Some(ref client) = GLOBAL_CLIENT {
            client.send_virtual_key(&key, pressed != 0);
        }
    }
}

#[cfg(feature = "android")]
#[no_mangle]
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

    unsafe {
        if let Some(ref client) = GLOBAL_CLIENT {
            client.set_touch_mode(mode);
        }
    }
}

#[cfg(feature = "android")]
#[no_mangle]
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativeGetTelemetryJson(
    env: JNIEnv,
    _class: JClass,
) -> jstring {
    let telemetry = unsafe {
        if let Some(ref client) = GLOBAL_CLIENT {
            client.telemetry_rx.borrow().clone()
        } else {
            BridgeTelemetry::default()
        }
    };

    let json = serde_json::to_string(&telemetry).unwrap_or_else(|_| "{}".to_string());
    match env.new_string(json) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}
