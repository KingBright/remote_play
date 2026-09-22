package com.remoteplay.client

import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.SystemClock

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import org.json.JSONObject
import java.nio.ByteBuffer
import java.nio.ByteOrder

data class EncodedVideoFrame(val data: ByteArray, val keyframe: Boolean, val ptsUs: Long, val dataOffset: Int)

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
    private var appContext: Context? = null
    private val pauseExecutor = java.util.concurrent.Executors.newSingleThreadExecutor()
    @Volatile var backgrounded = false
        private set
    private var surfaceUnavailable = false
    private var backgroundSinceMs = 0L
    private var lastConnection: Pair<String, String>? = null
    private var resumeAfterIdle: Pair<String, String>? = null
    private var connectionGeneration = 0L
    var backgroundKeepAliveSeconds = 300
        private set
    var manualMediaPaused = false
        private set
    var streamFps = 60
        private set
    var streamBitrateKbps = 20_000
        private set
    private val _mediaPaused = MutableStateFlow(false)
    val mediaPaused: StateFlow<Boolean> = _mediaPaused.asStateFlow()
    private val _mediaPausePending = MutableStateFlow(false)
    val mediaPausePending: StateFlow<Boolean> = _mediaPausePending.asStateFlow()

    init {
        try {
            System.loadLibrary("remote_play_android")
            isNativeLoaded = true
        } catch (e: UnsatisfiedLinkError) {
            isNativeLoaded = false
        }
    }

    val nativeAvailable: Boolean
        get() = isNativeLoaded

    fun initialize(context: Context): Boolean {
        appContext = context.applicationContext
        if (!isNativeLoaded) return false
        val initialized = nativeInit(context.filesDir.absolutePath)
        isNativeLoaded = initialized
        if (initialized) {
            val prefs = context.getSharedPreferences("stream", Context.MODE_PRIVATE)
            streamFps = prefs.getInt("fps", 60)
            streamBitrateKbps = prefs.getInt("bitrate_kbps", 20_000)
            backgroundKeepAliveSeconds = prefs.getInt("background_keepalive_seconds", 300).coerceIn(0, 86400)
            nativeUpdateStreamRates(streamFps, streamBitrateKbps)
        }
        return initialized
    }

    @Synchronized fun connect(deviceId: String, endpoint: String) {
        manualMediaPaused = false
        surfaceUnavailable = false
        resumeAfterIdle = null
        lastConnection = deviceId to endpoint
        val generation = ++connectionGeneration
        _sessionState.value = SessionState.CONNECTING
        pauseExecutor.execute {
            if (synchronized(this) { generation == connectionGeneration }) connectNow(deviceId, endpoint)
        }
    }

    private fun connectNow(deviceId: String, endpoint: String) {
        if (isNativeLoaded) {
            nativeSetNetworkEnabled(true)
            _sessionState.value = SessionState.CONNECTING
            appContext?.let { context ->
                val intent = Intent(context, SessionConnectionService::class.java)
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) context.startForegroundService(intent)
                else context.startService(intent)
            }
            nativeConnect(deviceId, endpoint)
            synchronized(this) { updateMediaPause() }
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

    @Synchronized fun disconnect() {
        ++connectionGeneration
        resumeAfterIdle = null
        lastConnection = null
        pauseExecutor.execute { disconnectNow() }
    }

    private fun disconnectNow() {
        if (isNativeLoaded) {
            nativeDisconnect()
            if (backgrounded) nativeSetNetworkEnabled(false)
        }
        _sessionState.value = SessionState.DISCONNECTED
        appContext?.let { it.stopService(Intent(it, SessionConnectionService::class.java)) }
    }

    @Synchronized fun setBackgrounded(value: Boolean) {
        if (value && !backgrounded) backgroundSinceMs = SystemClock.elapsedRealtime()
        backgrounded = value
        if (!value) {
            val resume = resumeAfterIdle
            resumeAfterIdle = null
            val generation = connectionGeneration
            if (resume != null) {
                lastConnection = resume
                manualMediaPaused = false
                surfaceUnavailable = false
                _sessionState.value = SessionState.CONNECTING
            }
            updateMediaPause()
            pauseExecutor.execute {
                if (isNativeLoaded) nativeSetNetworkEnabled(true)
                if (resume != null && synchronized(this) { generation == connectionGeneration }) {
                    connectNow(resume.first, resume.second)
                }
            }
        } else {
            updateMediaPause()
            if (lastConnection == null) {
                pauseExecutor.execute { if (isNativeLoaded) nativeSetNetworkEnabled(false) }
            }
        }
    }

    @Synchronized fun updateBackgroundKeepAlive(seconds: Int): String? {
        if (seconds !in 0..86400) return "Enter 0 (always keep) or up to 86400 seconds"
        backgroundKeepAliveSeconds = seconds
        appContext?.getSharedPreferences("stream", Context.MODE_PRIVATE)?.edit()
            ?.putInt("background_keepalive_seconds", seconds)?.apply()
        return null
    }

    /** Called by the foreground service; no alarm or wake lock is needed after expiry. */
    @Synchronized fun maintainBackgroundConnection(): Boolean {
        val seconds = appContext?.let { BackgroundConnectionPolicy.currentSeconds(it) } ?: backgroundKeepAliveSeconds
        if (!backgrounded || seconds == 0 || lastConnection == null ||
            SystemClock.elapsedRealtime() - backgroundSinceMs < seconds * 1000L) return false
        resumeAfterIdle = lastConnection
        lastConnection = null
        ++connectionGeneration
        pauseExecutor.execute { disconnectNow() }
        return true
    }

    @Synchronized fun setViewerSurfaceReady(ready: Boolean) {
        surfaceUnavailable = !ready
        updateMediaPause()
    }

    @Synchronized fun setManualMediaPaused(paused: Boolean) {
        manualMediaPaused = paused
        updateMediaPause()
    }

    @Synchronized private fun updateMediaPause() {
        val paused = manualMediaPaused || backgrounded || surfaceUnavailable
        _mediaPaused.value = paused
        // Ordered calls keep a delayed background callback from winning over resume.
        pauseExecutor.execute { if (isNativeLoaded) nativeSetMediaPaused(paused) }
    }

    fun updateStreamRates(fps: Int, bitrateKbps: Int): String? {
        if (fps <= 0 || bitrateKbps <= 0) return "FPS and kbps must be positive whole numbers"
        if (!isNativeLoaded) return "Native connection is unavailable"
        val error = nativeUpdateStreamRates(fps, bitrateKbps)
        if (error == null) {
            streamFps = fps
            streamBitrateKbps = bitrateKbps
            appContext?.getSharedPreferences("stream", Context.MODE_PRIVATE)?.edit()
                ?.putInt("fps", fps)?.putInt("bitrate_kbps", bitrateKbps)?.apply()
        }
        return error
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

    fun pollVideoFrame(): EncodedVideoFrame? {
        if (!isNativeLoaded) return null
        val packed = nativePollVideoFrame() ?: return null
        if (packed.size <= 9) return null
        val pts = ByteBuffer.wrap(packed, 1, 8).order(ByteOrder.LITTLE_ENDIAN).long
        return EncodedVideoFrame(packed, packed[0] != 0.toByte(), pts, 9)
    }

    fun requestKeyframe() {
        if (isNativeLoaded) nativeRequestKeyframe()
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
            _mediaPausePending.value = nativeMediaPausePending()
            _sessionState.value = when (nativeGetSessionState()) {
                0 -> SessionState.DISCONNECTED
                1 -> SessionState.CONNECTING
                2 -> SessionState.STREAMING
                3 -> SessionState.RECONNECTING
                else -> SessionState.ERROR
            }
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

    private external fun nativeInit(storageDir: String): Boolean
    private external fun nativeConnect(deviceId: String, endpoint: String)
    private external fun nativeDisconnect()
    private external fun nativeSetNetworkEnabled(enabled: Boolean)
    private external fun nativeSetMediaPaused(paused: Boolean)
    private external fun nativeMediaPausePending(): Boolean
    private external fun nativeUpdateStreamRates(fps: Int, bitrateKbps: Int): String?
    private external fun nativeSendTouch(actionCode: Int, pointerId: Int, normX: Float, normY: Float, pressure: Float)
    private external fun nativeSendVirtualKey(keyName: String, pressed: Boolean)
    private external fun nativeSetTouchMode(modeCode: Int)
    private external fun nativeSetScreenBounds(width: Int, height: Int)
    private external fun nativeGetTelemetryJson(): String?
    private external fun nativePollVideoFrame(): ByteArray?
    private external fun nativeRequestKeyframe()
    private external fun nativePollAudioPacket(): ByteArray?
    private external fun nativeGetDevicesJson(): String?
    private external fun nativeJoinPairingPayload(payload: String): String?
    private external fun nativeGetSessionState(): Int
}
