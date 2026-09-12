package com.remoteplay.client

import android.media.MediaCodec
import android.media.MediaFormat
import android.os.Build
import android.util.Log
import android.view.Surface

/**
 * Decodes the negotiated stream directly to a Surface. Device latency is measured at runtime.
 */
class MediaCodecPlayer(private val surface: Surface) {
    private var decoder: MediaCodec? = null
    var outputFrames: Long = 0
        private set
    @Synchronized
    fun start(width: Int = 1920, height: Int = 1080, mimeType: String = MediaFormat.MIMETYPE_VIDEO_HEVC) {
        stop()
        outputFrames = 0
        require(width > 0 && height > 0 && surface.isValid) { "Invalid video surface or dimensions" }
        var candidate: MediaCodec? = null
        try {
            val codec = MediaCodec.createDecoderByType(mimeType)
            candidate = codec
            val format = MediaFormat.createVideoFormat(mimeType, width, height).apply {
                if (Build.VERSION.SDK_INT >= 30 && codec.codecInfo
                        .getCapabilitiesForType(mimeType).isFeatureSupported("low-latency")) {
                    setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
                }
                setInteger(MediaFormat.KEY_PRIORITY, 0) // Realtime priority
            }
            codec.configure(format, surface, null, 0)
            codec.start()
            decoder = codec
        } catch (e: Exception) {
            runCatching { candidate?.release() }
                .onFailure { Log.w("RemotePlayVideo", "Decoder cleanup failed", it) }
            // Changing the decoder alone cannot turn an incoming HEVC stream into AVC.
            throw IllegalStateException("Cannot decode $mimeType on this device", e)
        }
    }

    /**
     * 将从 Rust 提取出的 NALU 数据块送入解码器
     */
    @Synchronized
    fun feedNalu(data: ByteArray, isKeyframe: Boolean, ptsUs: Long, dataOffset: Int = 0): Boolean {
        require(dataOffset in 0 until data.size) { "Invalid video payload offset" }
        val payloadSize = data.size - dataOffset
        val codec = decoder ?: return false
        drain()
        val inputIndex = codec.dequeueInputBuffer(0)
        if (inputIndex < 0) return false
        val input = checkNotNull(codec.getInputBuffer(inputIndex)) { "Missing decoder input buffer" }
        check(payloadSize <= input.capacity()) { "Video frame exceeds decoder input capacity" }
        input.clear()
        input.put(data, dataOffset, payloadSize)
        codec.queueInputBuffer(inputIndex, 0, payloadSize, ptsUs,
            if (isKeyframe) MediaCodec.BUFFER_FLAG_KEY_FRAME else 0)
        drain()
        return true
    }

    @Synchronized
    fun drain() {
        val codec = decoder ?: return
        val info = MediaCodec.BufferInfo()
        repeat(16) {
            val index = codec.dequeueOutputBuffer(info, 0)
            when {
                index >= 0 -> {
                    val render = surface.isValid && info.size > 0
                    codec.releaseOutputBuffer(index, render)
                    if (render) outputFrames++
                }
                index == MediaCodec.INFO_TRY_AGAIN_LATER -> return
            }
        }
    }

    @Synchronized
    fun stop() {
        val codec = decoder
        decoder = null
        runCatching { codec?.stop() }.onFailure { Log.w("RemotePlayVideo", "Decoder stop failed", it) }
        runCatching { codec?.release() }.onFailure { Log.w("RemotePlayVideo", "Decoder release failed", it) }
    }
}
