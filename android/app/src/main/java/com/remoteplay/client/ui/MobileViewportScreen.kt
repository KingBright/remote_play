package com.remoteplay.client.ui

import android.view.SurfaceView
import android.util.Log
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import com.remoteplay.client.AudioOpusPlayer
import com.remoteplay.client.MediaCodecPlayer
import com.remoteplay.client.RemotePlayClient
import com.remoteplay.client.SessionState
import com.remoteplay.client.TouchMode
import java.util.concurrent.atomic.AtomicBoolean

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun MobileViewportScreen(
    onDisconnect: () -> Unit
) {
    var touchMode by remember { mutableStateOf(TouchMode.DIRECT) }
    var isModifierBarVisible by remember { mutableStateOf(true) }
    val sessionState by RemotePlayClient.sessionState.collectAsState()
    var playbackError by remember { mutableStateOf<String?>(null) }
    var decodedFps by remember { mutableStateOf<Int?>(null) }

    BoxWithConstraints(
        modifier = Modifier
            .fillMaxSize()
            .background(Color.Black)
            .safeDrawingPadding()
    ) {
        // Keep the negotiated 1920x1080 stream and its input region at the same aspect ratio.
        AndroidView(
            modifier = Modifier
                .align(Alignment.Center)
                .width(minOf(maxWidth, maxHeight * (16f / 9f)))
                .aspectRatio(16f / 9f)
                .pointerInput(Unit) {
                    // 拦截手势并转化为 RemotePlay 统一触控协议
                    awaitEachGesture {
                        val down = awaitFirstDown()
                        var position = down.position
                        var released = false
                        fun send(action: Int) {
                            RemotePlayClient.sendTouchEvent(action, down.id.value.toInt(),
                                (position.x / size.width.coerceAtLeast(1)).coerceIn(0f, 1f),
                                (position.y / size.height.coerceAtLeast(1)).coerceIn(0f, 1f),
                                if (action <= 1) 1f else 0f)
                        }
                        send(0)
                        down.consume()
                        try {
                            while (true) {
                                val change = awaitPointerEvent().changes.firstOrNull { it.id == down.id } ?: break
                                position = change.position
                                change.consume()
                                if (!change.pressed) {
                                    send(2)
                                    released = true
                                    break
                                }
                                send(1)
                            }
                        } finally {
                            if (!released) send(3)
                        }
                    }
                },
            factory = { context ->
                SurfaceView(context).apply {
                    holder.addCallback(object : android.view.SurfaceHolder.Callback {
                        var feeder: Thread? = null
                        var running = AtomicBoolean(false)
                        override fun surfaceCreated(holder: android.view.SurfaceHolder) {
                            val active = AtomicBoolean(true)
                            running = active
                            playbackError = null
                            decodedFps = null
                            feeder = Thread {
                                val codec = MediaCodecPlayer(holder.surface)
                                val audioPlayer = AudioOpusPlayer()
                                var audioEnabled = true
                                fun report(message: String, error: Exception) {
                                    Log.e("RemotePlay", message, error)
                                    post { if (active.get()) playbackError = message }
                                }
                                try {
                                    codec.start(1920, 1080)
                                    var waitingForKeyframe = true
                                    var pending: com.remoteplay.client.EncodedVideoFrame? = null
                                    var pendingSince = 0L
                                    var lastKeyframeRequest = System.nanoTime()
                                    var sampleTime = lastKeyframeRequest
                                    var sampleFrames = 0L
                                    RemotePlayClient.requestKeyframe()
                                    while (active.get() && !Thread.currentThread().isInterrupted) {
                                        val now = System.nanoTime()
                                        if (pending == null) {
                                            val frame = RemotePlayClient.pollVideoFrame()
                                            if (frame != null && (!waitingForKeyframe || frame.keyframe)) {
                                                pending = frame
                                                pendingSince = now
                                            }
                                        }
                                        val frame = pending
                                        if (frame != null) {
                                            if (codec.feedNalu(frame.data, frame.keyframe, frame.ptsUs, frame.dataOffset)) {
                                                pending = null
                                                waitingForKeyframe = false
                                            } else if (now - pendingSince > 100_000_000L) {
                                                pending = null
                                                waitingForKeyframe = true
                                            }
                                        }
                                        codec.drain()
                                        if (now - sampleTime >= 500_000_000L) {
                                            val fps = ((codec.outputFrames - sampleFrames) * 1_000_000_000.0 / (now - sampleTime)).toInt()
                                            post { if (active.get()) decodedFps = fps }
                                            sampleTime = now
                                            sampleFrames = codec.outputFrames
                                        }
                                        if (waitingForKeyframe && now - lastKeyframeRequest >= 1_000_000_000L) {
                                            RemotePlayClient.requestKeyframe()
                                            lastKeyframeRequest = now
                                        }
                                        val audioPacket = RemotePlayClient.pollAudioPacket()
                                        if (audioEnabled) {
                                            try {
                                                if (audioPacket != null) audioPlayer.feed(audioPacket)
                                                audioPlayer.drain()
                                            } catch (e: Exception) {
                                                audioEnabled = false
                                                audioPlayer.stop()
                                                report("Audio unavailable: ${e.message}", e)
                                            }
                                        }
                                        if (frame == null || pending != null) Thread.sleep(4)
                                    }
                                } catch (_: InterruptedException) {
                                    Thread.currentThread().interrupt()
                                } catch (e: Exception) {
                                    report("Video unavailable: ${e.message}", e)
                                } finally {
                                    codec.stop()
                                    audioPlayer.stop()
                                }
                            }.apply { name = "remote-play-media"; start() }
                        }
                        override fun surfaceChanged(holder: android.view.SurfaceHolder, format: Int, width: Int, height: Int) {
                            RemotePlayClient.setScreenBounds(1920, 1080)
                        }
                        override fun surfaceDestroyed(holder: android.view.SurfaceHolder) {
                            running.set(false)
                            feeder?.interrupt()
                            feeder = null
                        }
                    })
                }
            }
        )

        playbackError?.let { message ->
            Text(message, color = Color.White,
                modifier = Modifier.align(Alignment.Center)
                    .background(Color.Black.copy(alpha = 0.85f)).padding(16.dp))
        }

        // 2. 顶部微型悬浮 Dynamic Touch Island
        FlowRow(
            modifier = Modifier
                .align(Alignment.TopCenter)
                .padding(top = 12.dp)
                .clip(RoundedCornerShape(16.dp))
                .background(ColorSurfaceCard)
                .border(1.dp, ColorBorderFine, RoundedCornerShape(16.dp))
                .padding(horizontal = 14.dp, vertical = 6.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
            horizontalArrangement = Arrangement.spacedBy(10.dp)
        ) {
            // 左侧：主机名与实时核心指标
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp)
            ) {
                Box(modifier = Modifier.size(6.dp).background(ColorAccentEmerald, CircleShape))
                Text(
                    text = "Remote session",
                    color = ColorTextPrimary,
                    fontSize = 11.sp,
                    fontWeight = FontWeight.Bold
                )
                Box(
                    modifier = Modifier
                        .clip(RoundedCornerShape(9999.dp))
                        .background(Color(0xFF132832))
                        .border(1.dp, ColorAccentCyan, RoundedCornerShape(9999.dp))
                        .padding(horizontal = 6.dp, vertical = 2.dp)
                ) {
                    Text(
                        text = if (sessionState == SessionState.RECONNECTING) "Reconnecting…"
                            else decodedFps?.let { "Decoded $it FPS" } ?: "Waiting for video",
                        color = ColorAccentCyan,
                        fontSize = 9.sp,
                        fontFamily = FontFamily.Monospace,
                        fontWeight = FontWeight.Bold
                    )
                }
            }

            // 细分隔线
            Box(modifier = Modifier.width(1.dp).height(12.dp).background(ColorBorderFine))

            // 快捷控制开关
            TouchIslandIconButton(
                text = if (touchMode == TouchMode.DIRECT) "TOUCH" else "TRACKPAD",
                active = touchMode == TouchMode.DIRECT
            ) {
                touchMode = if (touchMode == TouchMode.DIRECT) TouchMode.TRACKPAD else TouchMode.DIRECT
                RemotePlayClient.setTouchMode(touchMode)
            }

            TouchIslandIconButton(text = "MIC N/A", active = false, enabled = false) {}
            TouchIslandIconButton(text = "CLIP N/A", active = false, enabled = false) {}

            TouchIslandIconButton(text = "DISCONNECT", active = false, isDestructive = true) {
                onDisconnect()
            }
        }

        // 3. 底部浮动虚拟修饰键条 (Floating Modifier Bar)
        if (isModifierBarVisible) {
            Row(
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .padding(bottom = 12.dp)
                    .horizontalScroll(rememberScrollState())
                    .clip(RoundedCornerShape(9999.dp))
                    .background(ColorSurfaceCard)
                    .border(1.dp, ColorBorderFine, RoundedCornerShape(9999.dp))
                    .padding(horizontal = 10.dp, vertical = 6.dp),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalAlignment = Alignment.CenterVertically
            ) {
                listOf("Esc", "Ctrl", "Alt", "Win", "Shift", "Tab", "F5", "F11", "▲", "▼", "◄", "►").forEach { key ->
                    VirtualKeyPill(name = key) {
                        RemotePlayClient.sendVirtualKey(key, true)
                        RemotePlayClient.sendVirtualKey(key, false)
                    }
                }
            }
        }
    }
}

@Composable
fun TouchIslandIconButton(
    text: String,
    active: Boolean,
    isDestructive: Boolean = false,
    enabled: Boolean = true,
    onClick: () -> Unit
) {
    Box(
        modifier = Modifier
            .clip(RoundedCornerShape(9999.dp))
            .background(
                when {
                    isDestructive -> Color(0xFF3E1418)
                    active -> Color(0xFF132832)
                    else -> Color(0xFF22262E)
                }
            )
            .border(
                1.dp,
                when {
                    isDestructive -> Color(0xFFFF5252)
                    active -> ColorAccentCyan
                    else -> ColorBorderFine
                },
                RoundedCornerShape(9999.dp)
            )
            .clickable(enabled = enabled, onClick = onClick)
            .padding(horizontal = 8.dp, vertical = 4.dp)
    ) {
        Text(
            text = text,
            color = when {
                isDestructive -> Color(0xFFFF8A80)
                active -> ColorAccentCyan
                else -> ColorTextSecondary
            },
            fontSize = 9.sp,
            fontWeight = FontWeight.Bold,
            fontFamily = FontFamily.Monospace
        )
    }
}

@Composable
fun VirtualKeyPill(name: String, onClick: () -> Unit) {
    Box(
        modifier = Modifier
            .clip(RoundedCornerShape(6.dp))
            .background(Color(0xFF1C1E24))
            .border(1.dp, ColorBorderFine, RoundedCornerShape(6.dp))
            .clickable(onClick = onClick)
            .padding(horizontal = 8.dp, vertical = 6.dp),
        contentAlignment = Alignment.Center
    ) {
        Text(
            text = name,
            color = ColorTextPrimary,
            fontSize = 10.sp,
            fontFamily = FontFamily.Monospace,
            fontWeight = FontWeight.Medium
        )
    }
}
