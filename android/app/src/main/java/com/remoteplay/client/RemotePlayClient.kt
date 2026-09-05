package com.remoteplay.client

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import org.json.JSONObject

enum class SessionState {
    DISCONNECTED,
    CONNECTING,
    STREAMING,
    RECONNECTING,
    ERROR
}

enum class TouchMode(val code: Int) {
    DIRECT(0),
    TRACKPAD(1),
    GAMEPAD(2)
}

data class TelemetrySnapshot(
    val fps: Float = 0.0f,
    val latencyMs: Float = 0.0f,
    val jitterMs: Float = 0.0f,
    val packetLossPercent: Float = 0.0f,
    val videoBitrateKbps: Int = 0,
    val audioBitrateKbps: Int = 0,
    val controlBitrateKbps: Int = 0,
    val fileBitrateKbps: Int = 0,
    val transportHealthScore: Float = 0.0f
)

data class HostDevice(
    val id: String,
    val name: String,
    val endpoint: String,
    val scope: String,
    val canStream: Boolean,
    val online: Boolean
)

object RemotePlayClient {
    private val _sessionState = MutableStateFlow(SessionState.DISCONNECTED)
    val sessionState: StateFlow<SessionState> = _sessionState.asStateFlow()

    private val _telemetry = MutableStateFlow(TelemetrySnapshot())
    val telemetry: StateFlow<TelemetrySnapshot> = _telemetry.asStateFlow()

    private val _devices = MutableStateFlow<List<HostDevice>>(emptyList())
    val devices: StateFlow<List<HostDevice>> = _devices.asStateFlow()

    private var isNativeLoaded = false

    init {
        try {
            System.loadLibrary("remote_play_android")
            nativeInit()
            isNativeLoaded = true
        } catch (e: UnsatisfiedLinkError) {
            isNativeLoaded = false
        }
    }

    val nativeAvailable: Boolean
        get() = isNativeLoaded

    fun connect(deviceId: String, endpoint: String) {
        _sessionState.value = SessionState.CONNECTING
        if (isNativeLoaded) {
            nativeConnect(deviceId, endpoint)
            _sessionState.value = when (nativeGetSessionState()) {
                2 -> SessionState.STREAMING
                4 -> SessionState.ERROR
                1 -> SessionState.CONNECTING
                else -> SessionState.ERROR
            }
        } else {
            _sessionState.value = SessionState.ERROR
        }
    }

    fun disconnect() {
        if (isNativeLoaded) {
            nativeDisconnect()
        }
        _sessionState.value = SessionState.DISCONNECTED
    }

    fun sendTouchEvent(actionCode: Int, pointerId: Int, normX: Float, normY: Float, pressure: Float) {
        if (isNativeLoaded) {
            nativeSendTouch(actionCode, pointerId, normX, normY, pressure)
        }
    }

    fun sendVirtualKey(keyName: String, pressed: Boolean) {
        if (isNativeLoaded) {
            nativeSendVirtualKey(keyName, pressed)
        }
    }

    fun setTouchMode(mode: TouchMode) {
        if (isNativeLoaded) {
            nativeSetTouchMode(mode.code)
        }
    }

    fun setScreenBounds(width: Int, height: Int) {
        if (isNativeLoaded) {
            nativeSetScreenBounds(width, height)
        }
    }

    fun pollVideoNalu(): ByteArray? {
        if (!isNativeLoaded) return null
        return nativePollVideoNalu()
    }

    fun pollAudioPacket(): ByteArray? {
        if (!isNativeLoaded) return null
        return nativePollAudioPacket()
    }

    fun joinPairingPayload(raw: String): String {
        if (!isNativeLoaded) return "error:native library not loaded"
        return nativeJoinPairingPayload(raw) ?: "error:empty"
    }

    fun refreshDevices() {
        if (!isNativeLoaded) return
        val jsonStr = nativeGetDevicesJson() ?: return
        try {
            val array = org.json.JSONArray(jsonStr)
            val hosts = buildList {
                for (i in 0 until array.length()) {
                    val obj = array.getJSONObject(i)
                    add(
                        HostDevice(
                            id = obj.optString("device_id"),
                            name = obj.optString("display_name"),
                            endpoint = obj.optString("endpoint"),
                            scope = obj.optString("scope", "LAN"),
                            canStream = obj.optBoolean("can_stream", true),
                            online = obj.optBoolean("online", true)
                        )
                    )
                }
            }
            _devices.value = hosts
        } catch (_: Exception) {
        }
    }

    fun pollTelemetry(): TelemetrySnapshot {
        if (isNativeLoaded) {
            val jsonStr = nativeGetTelemetryJson()
            if (!jsonStr.isNullOrEmpty()) {
                try {
                    val obj = JSONObject(jsonStr)
                    val snap = TelemetrySnapshot(
                        fps = obj.optDouble("fps", 0.0).toFloat(),
                        latencyMs = obj.optDouble("latency_ms", 0.0).toFloat(),
                        jitterMs = obj.optDouble("jitter_ms", 0.0).toFloat(),
                        packetLossPercent = obj.optDouble("packet_loss_percent", 0.0).toFloat(),
                        videoBitrateKbps = obj.optInt("video_bitrate_kbps", 0),
                        audioBitrateKbps = obj.optInt("audio_bitrate_kbps", 0),
                        controlBitrateKbps = obj.optInt("control_bitrate_kbps", 0),
                        fileBitrateKbps = obj.optInt("file_bitrate_kbps", 0),
                        transportHealthScore = obj.optDouble("transport_health_score", 0.0).toFloat()
                    )
                    _telemetry.value = snap
                    return snap
                } catch (_: Exception) {}
            }
        }
        return _telemetry.value
    }

    private external fun nativeInit(): Boolean
    private external fun nativeConnect(deviceId: String, endpoint: String)
    private external fun nativeDisconnect()
    private external fun nativeSendTouch(actionCode: Int, pointerId: Int, normX: Float, normY: Float, pressure: Float)
    private external fun nativeSendVirtualKey(keyName: String, pressed: Boolean)
    private external fun nativeSetTouchMode(modeCode: Int)
    private external fun nativeSetScreenBounds(width: Int, height: Int)
    private external fun nativeGetTelemetryJson(): String?
    private external fun nativePollVideoNalu(): ByteArray?
    private external fun nativePollAudioPacket(): ByteArray?
    private external fun nativeGetDevicesJson(): String?
    private external fun nativeJoinPairingPayload(payload: String): String?
    private external fun nativeGetSessionState(): Int
}
