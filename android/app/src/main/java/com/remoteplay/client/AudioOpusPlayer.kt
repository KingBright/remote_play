package com.remoteplay.client

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.media.MediaCodec
import android.media.MediaFormat
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

    fun feed(packed: ByteArray) {
        if (packed.size < 7) return
        val header = ByteBuffer.wrap(packed, 0, 6).order(ByteOrder.LITTLE_ENDIAN)
        val rate = header.int
        val ch = header.short.toInt().coerceIn(1, 2)
        val payload = packed.copyOfRange(6, packed.size)
        ensureConfigured(rate, ch)
        val codec = decoder ?: return

        try {
            val inputIndex = codec.dequeueInputBuffer(2_000)
            if (inputIndex >= 0) {
                val input = codec.getInputBuffer(inputIndex) ?: return
                input.clear()
                input.put(payload)
                codec.queueInputBuffer(inputIndex, 0, payload.size, 0, 0)
            }

            val info = MediaCodec.BufferInfo()
            var outputIndex = codec.dequeueOutputBuffer(info, 0)
            while (outputIndex >= 0) {
                val output = codec.getOutputBuffer(outputIndex)
                if (output != null && info.size > 0) {
                    val pcm = ByteArray(info.size)
                    output.position(info.offset)
                    output.get(pcm)
                    track?.write(pcm, 0, pcm.size)
                }
                codec.releaseOutputBuffer(outputIndex, false)
                outputIndex = codec.dequeueOutputBuffer(info, 0)
            }
        } catch (_: Exception) {
        }
    }

    fun stop() {
        try {
            decoder?.stop()
            decoder?.release()
        } catch (_: Exception) {
        }
        try {
            track?.pause()
            track?.flush()
            track?.release()
        } catch (_: Exception) {
        }
        decoder = null
        track = null
        configured = false
    }

    private fun ensureConfigured(rate: Int, ch: Int) {
        if (configured && rate == sampleRate && ch == channels) return
        stop()
        sampleRate = rate
        channels = ch
        try {
            val format = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_OPUS, rate, ch)
            format.setByteBuffer("csd-0", ByteBuffer.wrap(opusHead(rate, ch)))
            val codec = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_AUDIO_OPUS)
            codec.configure(format, null, null, 0)
            codec.start()
            decoder = codec

            val channelMask = if (ch == 1) {
                AudioFormat.CHANNEL_OUT_MONO
            } else {
                AudioFormat.CHANNEL_OUT_STEREO
            }
            val minBuf = AudioTrack.getMinBufferSize(rate, channelMask, AudioFormat.ENCODING_PCM_16BIT)
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
                .also { it.play() }
            configured = true
        } catch (_: Exception) {
            configured = false
        }
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
