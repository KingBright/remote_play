package com.remoteplay.client

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.media.MediaCodec
import android.media.MediaFormat
import android.util.Log
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Decodes Opus packets from the native session and plays them through AudioTrack.
 */
class AudioOpusPlayer {
    private var decoder: MediaCodec? = null
    private var track: AudioTrack? = null
    private var sampleRate = 48000
    private var channels = 2
    private var configured = false

    @Synchronized
    fun feed(packed: ByteArray) {
        if (packed.size < 7) return
        val header = ByteBuffer.wrap(packed, 0, 6).order(ByteOrder.LITTLE_ENDIAN)
        val rate = header.int
        val ch = header.short.toInt()
        require(rate in setOf(8_000, 12_000, 16_000, 24_000, 48_000) && ch in 1..2) {
            "Unsupported Opus configuration: $rate Hz, $ch channels"
        }
        ensureConfigured(rate, ch)
        val codec = decoder ?: return
        drain()
        val inputIndex = codec.dequeueInputBuffer(0)
        if (inputIndex >= 0) {
            val input = checkNotNull(codec.getInputBuffer(inputIndex))
            check(packed.size - 6 <= input.capacity()) { "Opus packet exceeds decoder capacity" }
            input.clear()
            input.put(packed, 6, packed.size - 6)
            codec.queueInputBuffer(inputIndex, 0, packed.size - 6, 0, 0)
        }
        drain()
    }

    @Synchronized
    fun drain() {
        val codec = decoder ?: return
        val info = MediaCodec.BufferInfo()
        repeat(16) {
            val index = codec.dequeueOutputBuffer(info, 0)
            when {
                index >= 0 -> try {
                    val output = codec.getOutputBuffer(index)
                    if (output != null && info.size > 0) {
                        output.position(info.offset)
                        output.limit(info.offset + info.size)
                        // Never block video feeding behind speaker output. Drop excess PCM.
                        val written = track?.write(output, info.size, AudioTrack.WRITE_NON_BLOCKING) ?: 0
                        check(written >= 0) { "Audio output failed: $written" }
                    }
                } finally {
                    codec.releaseOutputBuffer(index, false)
                }
                index == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED -> {
                    val format = codec.outputFormat
                    configureTrack(format.getInteger(MediaFormat.KEY_SAMPLE_RATE),
                        format.getInteger(MediaFormat.KEY_CHANNEL_COUNT))
                }
                index == MediaCodec.INFO_TRY_AGAIN_LATER -> return
            }
        }
    }

    @Synchronized
    fun stop() {
        val codec = decoder
        decoder = null
        configured = false
        runCatching { codec?.stop() }.onFailure { Log.w("RemotePlayAudio", "Decoder stop failed", it) }
        runCatching { codec?.release() }.onFailure { Log.w("RemotePlayAudio", "Decoder release failed", it) }
        releaseTrack()
    }

    private fun releaseTrack() {
        val output = track
        track = null
        runCatching { output?.pause() }.onFailure { Log.w("RemotePlayAudio", "Audio pause failed", it) }
        runCatching { output?.flush() }.onFailure { Log.w("RemotePlayAudio", "Audio flush failed", it) }
        runCatching { output?.release() }.onFailure { Log.w("RemotePlayAudio", "Audio release failed", it) }
    }

    private fun ensureConfigured(rate: Int, ch: Int) {
        if (configured && rate == sampleRate && ch == channels) return
        stop()
        sampleRate = rate
        channels = ch
        try {
            val format = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_OPUS, rate, ch)
            format.setByteBuffer("csd-0", ByteBuffer.wrap(opusHead(rate, ch)))
            format.setByteBuffer("csd-1", ByteBuffer.allocate(8).order(ByteOrder.nativeOrder()).putLong(0).apply { flip() })
            format.setByteBuffer("csd-2", ByteBuffer.allocate(8).order(ByteOrder.nativeOrder()).putLong(80_000_000).apply { flip() })
            val codec = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_AUDIO_OPUS)
            decoder = codec // Own it before configure/start, which can throw.
            codec.configure(format, null, null, 0)
            codec.start()
            configured = true
        } catch (e: Exception) {
            stop()
            throw IllegalStateException("Cannot initialize Opus audio", e)
        }
    }

    private fun configureTrack(rate: Int, ch: Int) {
            require(rate > 0 && ch in 1..2) { "Unsupported decoded audio format" }
            releaseTrack()
            val channelMask = if (ch == 1) {
                AudioFormat.CHANNEL_OUT_MONO
            } else {
                AudioFormat.CHANNEL_OUT_STEREO
            }
            val minBuf = AudioTrack.getMinBufferSize(rate, channelMask, AudioFormat.ENCODING_PCM_16BIT)
            check(minBuf > 0) { "Audio output buffer unavailable" }
            track = AudioTrack.Builder()
                .setAudioAttributes(
                    AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MOVIE)
                        .build()
                )
                .setAudioFormat(
                    AudioFormat.Builder()
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setSampleRate(rate)
                        .setChannelMask(channelMask)
                        .build()
                )
                .setBufferSizeInBytes(minBuf * 2)
                .setTransferMode(AudioTrack.MODE_STREAM)
                .build()
            check(track?.state == AudioTrack.STATE_INITIALIZED) { "Audio output unavailable" }
            track?.play()
    }

    private fun opusHead(rate: Int, channels: Int): ByteArray {
        val buf = ByteBuffer.allocate(19).order(ByteOrder.LITTLE_ENDIAN)
        buf.put("OpusHead".toByteArray())
        buf.put(1)
        buf.put(channels.toByte())
        buf.putShort(0)
        buf.putInt(rate)
        buf.putShort(0)
        buf.put(0)
        return buf.array()
    }
}
