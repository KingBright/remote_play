package com.remoteplay.client

import android.app.*
import android.content.Intent
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.*
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.os.*
import android.view.Surface
import org.json.JSONObject
import java.io.File
import java.util.concurrent.atomic.AtomicBoolean

/** Owns one OS authorization and one VirtualDisplay for its entire lifetime. */
class ProjectionService : Service() {
    private val running = AtomicBoolean(false)
    private var worker: Thread? = null
    private var projection: MediaProjection? = null
    @Volatile private var sourceWidth = 1080
    @Volatile private var sourceHeight = 1920
    @Volatile private var audioAllowed = false
    private val audioRunning = AtomicBoolean(false)
    private var audioWorker: Thread? = null

    override fun onBind(intent: Intent?) = null
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == "stop") { stopSelf(); return START_NOT_STICKY }
        if (running.get()) return START_NOT_STICKY
        val consent = if (Build.VERSION.SDK_INT >= 33) intent?.getParcelableExtra("consent", Intent::class.java) else @Suppress("DEPRECATION") intent?.getParcelableExtra("consent")
        if (consent == null) { stopSelf(); return START_NOT_STICKY }
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(NotificationChannel("projection", "Screen sharing", NotificationManager.IMPORTANCE_LOW))
        val stop = PendingIntent.getService(this, 2, Intent(this, ProjectionService::class.java).setAction("stop"), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        val open = PendingIntent.getActivity(this, 3, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        startForeground(8124, Notification.Builder(this, "projection").setSmallIcon(android.R.drawable.stat_sys_upload)
            .setContentTitle("RemotePlay screen sharing").setContentText("Only paired devices can connect on port 5001")
            .setContentIntent(open).setOngoing(true).addAction(Notification.Action.Builder(null, "Stop sharing", stop).build()).build())
        try {
            val metrics = resources.displayMetrics
            sourceWidth = metrics.widthPixels; sourceHeight = metrics.heightPixels
            audioAllowed = intent?.getBooleanExtra("audio", false) == true && Build.VERSION.SDK_INT >= 29
            val capture = checkNotNull(getSystemService(MediaProjectionManager::class.java).getMediaProjection(intent?.getIntExtra("result", Activity.RESULT_CANCELED) ?: Activity.RESULT_CANCELED, consent))
            projection = capture
            capture.registerCallback(object : MediaProjection.Callback() {
                override fun onStop() { running.set(false); stopSelf() }
                override fun onCapturedContentResize(width: Int, height: Int) { if (width > 1 && height > 1) { sourceWidth = width; sourceHeight = height } }
            }, Handler(Looper.getMainLooper()))
            running.set(true)
            worker = Thread({ publish(capture, metrics.densityDpi) }, "remote-projection").apply { start() }
        } catch (e: Exception) { PublisherClient.status(e.message ?: "Screen sharing failed", false); stopSelf() }
        return START_NOT_STICKY
    }

    private fun publish(capture: MediaProjection, density: Int) {
        var display: VirtualDisplay? = null
        var codec: MediaCodec? = null
        var surface: Surface? = null
        var sourceSize = 0 to 0
        var formatKey = ""
        var codecHeader = ByteArray(0)
        var keyframe = -1L
        var demand = JSONObject()
        var nextDemand = 0L
        try {
            val directory = File(filesDir, "received").apply { mkdirs() }
            PublisherClient.action(0, JSONObject().put("bind", "0.0.0.0:5001").put("directory", directory.absolutePath).put("audio", audioAllowed).toString())
            // Null surface suspends capture without releasing the authorization.
            display = capture.createVirtualDisplay("RemotePlay", sourceWidth, sourceHeight, density,
                DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR, null, null, Handler(Looper.getMainLooper()))
            val addresses = java.util.Collections.list(java.net.NetworkInterface.getNetworkInterfaces()).filter { it.isUp && !it.isLoopback }
                .flatMap { java.util.Collections.list(it.inetAddresses) }.filterIsInstance<java.net.Inet4Address>().joinToString { "${it.hostAddress}:5001" }
            PublisherClient.status("Sharing enabled · $addresses", true)
            val info = MediaCodec.BufferInfo()
            val hardwareEncoders = MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos.filter {
                it.isEncoder && it.supportedTypes.any { type -> type.equals(MediaFormat.MIMETYPE_VIDEO_HEVC, true) } &&
                    (if (Build.VERSION.SDK_INT >= 29) it.isHardwareAccelerated else !it.name.startsWith("OMX.google.") && !it.name.startsWith("c2.android."))
            }
            var selectionKey = ""
            var selection: Pair<MediaCodecInfo, VideoDimensions>? = null
            while (running.get()) {
                if (SystemClock.elapsedRealtime() >= nextDemand) {
                    demand = JSONObject(PublisherClient.action(2)); nextDemand = SystemClock.elapsedRealtime() + 100
                }
                if (sourceSize != (sourceWidth to sourceHeight)) {
                    sourceSize = sourceWidth to sourceHeight
                    PublisherClient.action(3, JSONObject().put("width", sourceWidth).put("height", sourceHeight).toString())
                }
                val active = demand.optBoolean("active")
                if (active) {
                    val targetW = demand.optInt("width", 1280); val targetH = demand.optInt("height", 720)
                    val fps = demand.optInt("fps", 30); val bitrate = demand.optInt("bitrate_kbps", 5000)
                    val bits = Math.multiplyExact(bitrate, 1000)
                    val requested = "$sourceWidth:$sourceHeight:$targetW:$targetH:$fps:$bitrate"
                    if (selectionKey != requested) {
                        selection = hardwareEncoders.asSequence().mapNotNull { encoderInfo ->
                            val cap = encoderInfo.getCapabilitiesForType(MediaFormat.MIMETYPE_VIDEO_HEVC).videoCapabilities
                            val size = VideoDimensions.fitEncoder(sourceWidth, sourceHeight, targetW, targetH, cap.widthAlignment, cap.heightAlignment)
                            if (size != null && cap.areSizeAndRateSupported(size.width, size.height, fps.toDouble()) && cap.bitrateRange.contains(bits)) encoderInfo to size else null
                        }.firstOrNull()
                        selectionKey = requested
                    }
                    val supported = selection ?: error("No hardware HEVC encoder supports this resolution, frame rate and bitrate")
                    val width = supported.second.width; val height = supported.second.height
                    val key = "$width:$height:$fps:$bitrate"
                    if (codec == null || key != formatKey) {
                        display.surface = null
                        codec?.let { runCatching { it.stop() }; it.release() }; codec = null
                        surface?.release(); surface = null
                        val candidate = MediaCodec.createByCodecName(supported.first.name)
                        codec = candidate
                        if (Build.VERSION.SDK_INT >= 29) check(candidate.codecInfo.isHardwareAccelerated) { "A hardware HEVC encoder is required" }
                        val capabilities = candidate.codecInfo.getCapabilitiesForType(MediaFormat.MIMETYPE_VIDEO_HEVC).videoCapabilities
                        check(capabilities.areSizeAndRateSupported(width, height, fps.toDouble())) { "Hardware cannot encode ${width}×${height} at $fps fps" }
                        check(capabilities.bitrateRange.contains(bits)) { "Bitrate is outside this encoder's supported range" }
                        val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_HEVC, width, height).apply {
                            setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
                            setInteger(MediaFormat.KEY_BIT_RATE, bits); setInteger(MediaFormat.KEY_FRAME_RATE, fps)
                            setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 2); setInteger(MediaFormat.KEY_PRIORITY, 0)
                            if (Build.VERSION.SDK_INT >= 29) setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0)
                        }
                        candidate.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
                        surface = candidate.createInputSurface(); candidate.start()
                        display.resize(width, height, density); display.surface = surface
                        formatKey = key; codecHeader = ByteArray(0); keyframe = -1
                    }
                    val encoder = checkNotNull(codec)
                    if (keyframe != demand.optLong("keyframe")) {
                        keyframe = demand.optLong("keyframe")
                        encoder.setParameters(Bundle().apply { putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0) })
                    }
                    repeat(8) {
                        val index = encoder.dequeueOutputBuffer(info, 0)
                        if (index == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                            encoder.outputFormat.getByteBuffer("csd-0")?.let { buffer -> codecHeader = ByteArray(buffer.remaining()).also { buffer.get(it) } }
                        } else if (index >= 0) {
                            try {
                                if (info.size > 0) {
                                    val output = checkNotNull(encoder.getOutputBuffer(index)); output.position(info.offset); output.limit(info.offset + info.size)
                                    val bytes = ByteArray(info.size); output.get(bytes)
                                    if (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0) codecHeader = bytes
                                    else {
                                        val payload = if (info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME != 0) codecHeader + bytes else bytes
                                        PublisherClient.nativeFrame(payload, info.presentationTimeUs)
                                    }
                                }
                            } finally { encoder.releaseOutputBuffer(index, false) }
                        }
                    }
                } else if (codec != null) {
                    display.surface = null; runCatching { codec?.stop() }; codec?.release(); codec = null
                    surface?.release(); surface = null
                }
                val wantsAudio = audioAllowed && demand.optBoolean("audio")
                if (wantsAudio && !audioRunning.get()) startAudio(capture)
                if (!wantsAudio && audioRunning.get()) stopAudio()
                Thread.sleep(if (active) 4 else 100)
            }
        } catch (e: Exception) {
            if (running.get()) { PublisherClient.status(e.message ?: "Projection stopped", false); runCatching { PublisherClient.action(4, e.message ?: "Projection stopped") } }
        } finally {
            running.set(false); stopAudio()
            display?.surface = null; display?.release()
            runCatching { codec?.stop() }; codec?.release(); surface?.release()
            runCatching { PublisherClient.action(1) }
            if (PublisherClient.active) PublisherClient.status("Screen sharing stopped", false)
            stopSelf()
        }
    }

    @Suppress("MissingPermission")
    private fun startAudio(capture: MediaProjection) {
        if (Build.VERSION.SDK_INT < 29) return
        audioRunning.set(true)
        audioWorker = Thread({
            var record: AudioRecord? = null
            try {
                val config = AudioPlaybackCaptureConfiguration.Builder(capture)
                    .addMatchingUsage(AudioAttributes.USAGE_MEDIA).addMatchingUsage(AudioAttributes.USAGE_GAME)
                    .excludeUid(android.os.Process.myUid()).build()
                val format = AudioFormat.Builder().setSampleRate(48000).setEncoding(AudioFormat.ENCODING_PCM_16BIT).setChannelMask(AudioFormat.CHANNEL_IN_STEREO).build()
                record = AudioRecord.Builder().setAudioPlaybackCaptureConfig(config).setAudioFormat(format)
                    .setBufferSizeInBytes(maxOf(7680, AudioRecord.getMinBufferSize(48000, AudioFormat.CHANNEL_IN_STEREO, AudioFormat.ENCODING_PCM_16BIT))).build()
                check(record.state == AudioRecord.STATE_INITIALIZED) { "Playback audio capture is unavailable" }
                record.startRecording()
                val samples = ShortArray(1920)
                var filled = 0
                while (audioRunning.get() && running.get()) {
                    val count = record.read(samples, filled, samples.size - filled, AudioRecord.READ_BLOCKING)
                    check(count >= 0) { "Audio capture failed: $count" }; filled += count
                    if (filled == samples.size) { check(PublisherClient.nativePcm(samples)) { "Opus encoder failed" }; filled = 0 }
                }
            } catch (e: Exception) {
                if (audioRunning.get()) { audioAllowed = false; PublisherClient.status("Sharing video · ${e.message}") }
            } finally { runCatching { record?.stop() }; record?.release(); audioRunning.set(false) }
        }, "projection-audio").apply { start() }
    }
    private fun stopAudio() { audioRunning.set(false); audioWorker?.join(500); audioWorker = null }
    override fun onDestroy() {
        running.set(false); worker?.interrupt(); runCatching { projection?.stop() }; projection = null
        super.onDestroy()
    }
}
