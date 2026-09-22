#[cfg(feature = "android")]
use crate::{RemoteBridgeClient, configure_device_group_dir, touch_mapper::TouchMode};
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
static VIEWER_NETWORK_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

static WORKSPACE: std::sync::Mutex<Option<Arc<crate::workspace_bridge::MobileWorkspace>>> =
    std::sync::Mutex::new(None);
static PUBLISHER: std::sync::Mutex<Option<Arc<crate::publisher_bridge::MobilePublisher>>> =
    std::sync::Mutex::new(None);

fn refresh_network_ownership() {
    client().set_network_enabled(
        VIEWER_NETWORK_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
            || crate::PUBLISHING_PORT.load(std::sync::atomic::Ordering::Relaxed) != 0
            || WORKSPACE.lock().unwrap().is_some(),
    );
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_PublisherClient_nativeAction(
    mut env: JNIEnv,
    _class: JClass,
    operation: jint,
    payload: JString,
) -> jstring {
    let result = (|| -> Result<String, String> {
        let value: String = env.get_string(&payload).map_err(|e| e.to_string())?.into();
        if operation == 0 {
            let value: serde_json::Value =
                serde_json::from_str(&value).map_err(|e| e.to_string())?;
            let bind = value["bind"]
                .as_str()
                .unwrap_or("0.0.0.0:5001")
                .parse()
                .map_err(|_| "invalid listen address")?;
            let directory = value["directory"]
                .as_str()
                .ok_or("missing receive directory")?
                .into();
            if PUBLISHER.lock().unwrap().is_some() {
                return Err("screen sharing is already running".into());
            }
            let publisher =
                client()
                    .runtime()
                    .block_on(crate::publisher_bridge::MobilePublisher::start(
                        bind,
                        directory,
                        value["audio"].as_bool().unwrap_or(false),
                    ))?;
            let address = publisher.address.to_string();
            crate::PUBLISHING_PORT.store(
                publisher.address.port(),
                std::sync::atomic::Ordering::Relaxed,
            );
            *PUBLISHER.lock().unwrap() = Some(Arc::new(publisher));
            client().set_network_enabled(true);
            let _ = client().network_reload_tx.send(());
            return Ok(address);
        }
        if operation == 1 {
            PUBLISHER.lock().unwrap().take();
            crate::PUBLISHING_PORT.store(0, std::sync::atomic::Ordering::Relaxed);
            refresh_network_ownership();
            let _ = client().network_reload_tx.send(());
            return Ok(String::new());
        }
        let publisher = PUBLISHER
            .lock()
            .unwrap()
            .clone()
            .ok_or("screen sharing is not running")?;
        match operation {
            2 if publisher.is_finished() => {
                Err("Screen sharing ended; request a new system authorization".into())
            }
            2 => serde_json::to_string(&publisher.demand()).map_err(|e| e.to_string()),
            3 => {
                let value: serde_json::Value =
                    serde_json::from_str(&value).map_err(|e| e.to_string())?;
                publisher.set_source_size(
                    value["width"].as_u64().unwrap_or(0) as u32,
                    value["height"].as_u64().unwrap_or(0) as u32,
                );
                Ok(String::new())
            }
            4 => {
                publisher.fail(value);
                Ok(String::new())
            }
            _ => Err("unknown publisher command".into()),
        }
    })();
    let value = match result {
        Ok(value) => value,
        Err(error) => serde_json::json!({"error":error}).to_string(),
    };
    env.new_string(value)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_PublisherClient_nativeFrame(
    env: JNIEnv,
    _class: JClass,
    bytes: jni::objects::JByteArray,
    pts: jni::sys::jlong,
) {
    if env
        .get_array_length(&bytes)
        .is_ok_and(|length| length > 0 && length <= 8 * 1024 * 1024)
        && let Ok(bytes) = env.convert_byte_array(&bytes)
        && let Some(publisher) = PUBLISHER.lock().unwrap().clone()
    {
        publisher.frame(bytes, pts, false);
    }
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_PublisherClient_nativePcm(
    env: JNIEnv,
    _class: JClass,
    samples: jni::objects::JShortArray,
) -> jboolean {
    static ENCODER: OnceLock<std::sync::Mutex<Option<opus::Encoder>>> = OnceLock::new();
    if env.get_array_length(&samples).ok() != Some(1920) {
        return 0;
    }
    let mut pcm = [0i16; 1920];
    if env.get_short_array_region(&samples, 0, &mut pcm).is_err() {
        return 0;
    }
    let mut encoder = ENCODER
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap();
    if encoder.is_none() {
        *encoder = opus::Encoder::new(48000, opus::Channels::Stereo, opus::Application::Audio).ok();
    }
    let Some(encoder) = encoder.as_mut() else {
        return 0;
    };
    let mut bytes = [0u8; 4000];
    let Ok(length) = encoder.encode(&pcm, &mut bytes) else {
        return 0;
    };
    if let Some(publisher) = PUBLISHER.lock().unwrap().clone() {
        publisher.frame(bytes[..length].to_vec(), 0, true);
    }
    1
}

/// JSON control stays off Android's main thread; media uses separate bounded queues.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_WorkspaceClient_nativeAction(
    mut env: JNIEnv,
    _class: JClass,
    operation: jint,
    payload: JString,
) -> jstring {
    let result =
        (|| -> Result<String, String> {
            let payload: String = env.get_string(&payload).map_err(|e| e.to_string())?.into();
            let client = client();
            if operation == 0 {
                let value: serde_json::Value =
                    serde_json::from_str(&payload).map_err(|e| e.to_string())?;
                let endpoint = value["endpoint"]
                    .as_str()
                    .ok_or("missing endpoint")?
                    .parse()
                    .map_err(|_| "invalid endpoint")?;
                let directory = value["directory"]
                    .as_str()
                    .ok_or("missing receive directory")?
                    .into();
                let workspace = client.runtime().block_on(
                    crate::workspace_bridge::MobileWorkspace::connect(endpoint, directory),
                )?;
                *WORKSPACE.lock().unwrap() = Some(Arc::new(workspace));
                refresh_network_ownership();
                return Ok(String::new());
            }
            if operation == 1 {
                WORKSPACE.lock().unwrap().take();
                refresh_network_ownership();
                return Ok(String::new());
            }
            let workspace = WORKSPACE
                .lock()
                .unwrap()
                .clone()
                .ok_or("workspace disconnected")?;
            match operation {
                2 => client.runtime().block_on(workspace.command(&payload))?,
                3 => client.runtime().block_on(workspace.settings(&payload))?,
                4 => client
                    .runtime()
                    .block_on(workspace.send_file(payload.into()))?,
                5 => return Ok(workspace.events()),
                6 => client.runtime().block_on(
                    workspace.keyframe(payload.parse().map_err(|_| "invalid subscription id")?),
                )?,
                7 => client
                    .runtime()
                    .block_on(workspace.send_clipboard(&payload))?,
                _ => return Err("unknown workspace operation".into()),
            }
            Ok(String::new())
        })();
    let output = match result {
        Ok(value) => value,
        Err(error) => serde_json::json!({"error":error}).to_string(),
    };
    env.new_string(output)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_WorkspaceClient_nativeMedia(
    env: JNIEnv,
    _class: JClass,
    id: jint,
    audio: jboolean,
) -> jbyteArray {
    let workspace = WORKSPACE.lock().unwrap().clone();
    let data = workspace.and_then(|workspace| {
        if audio != 0 {
            workspace.audio(id as u32)
        } else {
            workspace.video(id as u32)
        }
    });
    data.and_then(|bytes| env.byte_array_from_slice(&bytes).ok())
        .map(|a| a.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[cfg(feature = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativeSetNetworkEnabled(
    _env: JNIEnv,
    _class: JClass,
    enabled: jboolean,
) {
    VIEWER_NETWORK_ENABLED.store(enabled != 0, std::sync::atomic::Ordering::Relaxed);
    refresh_network_ownership();
}

#[cfg(feature = "android")]
fn client() -> Arc<RemoteBridgeClient> {
    GLOBAL_CLIENT
        .get_or_init(|| Arc::new(RemoteBridgeClient::new()))
        .clone()
}

#[cfg(feature = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativeInit(
    mut env: JNIEnv,
    _class: JClass,
    storage_dir: JString,
) -> jboolean {
    let storage_dir: String = match env.get_string(&storage_dir) {
        Ok(value) => value.into(),
        Err(_) => return 0,
    };
    let device_group_dir = std::path::PathBuf::from(storage_dir)
        .join("remote-play")
        .join("device-group");
    if configure_device_group_dir(device_group_dir).is_err() && GLOBAL_CLIENT.get().is_none() {
        return 0;
    }
    let _ = client();
    let store = remote_core::mesh::AppPrivateMeshConfigStore::new(crate::bridge_device_group_dir());
    let Ok(identity) = store.load_or_generate("RemotePlay Android") else {
        return 0;
    };
    remote_core::session_crypto::use_paired_session_secret(identity.network_secret.expose_secret());
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
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativeSetMediaPaused(
    _env: JNIEnv,
    _class: JClass,
    paused: jboolean,
) {
    client().set_media_paused(paused != 0);
}

#[cfg(feature = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativeMediaPausePending(
    _env: JNIEnv,
    _class: JClass,
) -> jboolean {
    client().host_stats.media_pause.is_pending() as jboolean
}

#[cfg(feature = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoteplay_client_RemotePlayClient_nativeUpdateStreamRates(
    env: JNIEnv,
    _class: JClass,
    fps: jint,
    bitrate_kbps: jint,
) -> jstring {
    let result = if fps <= 0 || bitrate_kbps <= 0 {
        Err("FPS and kbps must be positive whole numbers".to_string())
    } else {
        client().update_stream_rates(fps as u32, bitrate_kbps as u32)
    };
    match result {
        Ok(()) => std::ptr::null_mut(),
        Err(err) => env
            .new_string(err)
            .map(|s| s.into_raw())
            .unwrap_or(std::ptr::null_mut()),
    }
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
        Ok(message) => {
            if let Some(publisher) = PUBLISHER.lock().unwrap().as_ref() {
                publisher.fail("Device group changed; restart screen sharing".into());
            }
            message
        }
        Err(err) => format!("error:{err}"),
    };
    match env.new_string(result) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}
