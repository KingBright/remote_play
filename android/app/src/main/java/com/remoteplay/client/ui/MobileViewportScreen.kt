package com.remoteplay.client.ui

import android.view.SurfaceView
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGestures
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
import com.remoteplay.client.TelemetrySnapshot
import com.remoteplay.client.TouchMode
import kotlinx.coroutines.delay

@Composable
fun MobileViewportScreen(
    telemetry: TelemetrySnapshot,
    onDisconnect: () -> Unit
) {
    var touchMode by remember { mutableStateOf(TouchMode.DIRECT) }
    var micEnabled by remember { mutableStateOf(false) }
    var clipboardSync by remember { mutableStateOf(true) }
    var isModifierBarVisible by remember { mutableStateOf(true) }
    var liveTelemetry by remember { mutableStateOf(telemetry) }

    LaunchedEffect(Unit) {
        while (true) {
            liveTelemetry = RemotePlayClient.pollTelemetry()
            delay(500)
        }
    }

    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(Color.Black)
    ) {
        // 1. 100% 满屏硬件解码 SurfaceView 渲染层
        AndroidView(
            modifier = Modifier
                .fillMaxSize()
                .pointerInput(Unit) {
                    // 拦截手势并转化为 RemotePlay 统一触控协议
                    detectDragGestures(
                        onDragStart = { offset ->
                            val normX = (offset.x / size.width).coerceIn(0f, 1f)
                            val normY = (offset.y / size.height).coerceIn(0f, 1f)
                            RemotePlayClient.sendTouchEvent(0, 0, normX, normY, 1.0f)
                        },
                        onDrag = { change, _ ->
                            change.consume()
                            val normX = (change.position.x / size.width).coerceIn(0f, 1f)
                            val normY = (change.position.y / size.height).coerceIn(0f, 1f)
                            RemotePlayClient.sendTouchEvent(1, 0, normX, normY, 1.0f)
                        },
                        onDragEnd = {
                            RemotePlayClient.sendTouchEvent(2, 0, 0f, 0f, 0.0f)
                        },
                        onDragCancel = {
                            RemotePlayClient.sendTouchEvent(3, 0, 0f, 0f, 0.0f)
                        }
                    )
                },
            factory = { context ->
                SurfaceView(context).apply {
                    holder.addCallback(object : android.view.SurfaceHolder.Callback {
                        var player: MediaCodecPlayer? = null
                        var audio: AudioOpusPlayer? = null
                        var feeder: Thread? = null
                        @Volatile var running = false
                        override fun surfaceCreated(holder: android.view.SurfaceHolder) {
                            val codec = MediaCodecPlayer(holder.surface).apply {
                                start(1920, 1080)
                            }
                            val audioPlayer = AudioOpusPlayer()
                            player = codec
                            audio = audioPlayer
                            running = true
                            feeder = Thread {
                                var pts = 0L
                                while (running) {
                                    val nalu = RemotePlayClient.pollVideoNalu()
                                    if (nalu != null) {
                                        codec.feedNalu(nalu, nalu.size > 4, pts)
                                        pts += 16_000
                                    }
                                    val audioPacket = RemotePlayClient.pollAudioPacket()
                                    if (audioPacket != null) {
                                        audioPlayer.feed(audioPacket)
                                    }
                                    if (nalu == null && audioPacket == null) {
                                        try { Thread.sleep(4) } catch (_: InterruptedException) { break }
                                    }
                                }
                            }.also { it.start() }
                        }
                        override fun surfaceChanged(holder: android.view.SurfaceHolder, format: Int, width: Int, height: Int) {
                            RemotePlayClient.setScreenBounds(width, height)
                        }
                        override fun surfaceDestroyed(holder: android.view.SurfaceHolder) {
                            running = false
                            feeder?.interrupt()
                            feeder = null
                            player?.stop()
                            player = null
                            audio?.stop()
                            audio = null
                        }
                    })
                }
            }
        )

        // 2. 顶部微型悬浮 Dynamic Touch Island
        Row(
            modifier = Modifier
                .align(Alignment.TopCenter)
                .padding(top = 12.dp)
                .clip(RoundedCornerShape(9999.dp))
                .background(ColorSurfaceCard)
                .border(1.dp, ColorBorderFine, RoundedCornerShape(9999.dp))
                .padding(horizontal = 14.dp, vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp)
        ) {
            // 左侧：主机名与实时核心指标
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp)
            ) {
                Box(modifier = Modifier.size(6.dp).background(ColorAccentEmerald, CircleShape))
                Text(
                    text = "Gaming Rig RTX 4090",
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
                        text = "${liveTelemetry.fps.toInt()} FPS · ${liveTelemetry.latencyMs}ms",
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

            TouchIslandIconButton(text = "MIC", active = micEnabled) {
                micEnabled = !micEnabled
            }

            TouchIslandIconButton(text = "CLIP", active = clipboardSync) {
                clipboardSync = !clipboardSync
            }

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
            .clickable(onClick = onClick)
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
