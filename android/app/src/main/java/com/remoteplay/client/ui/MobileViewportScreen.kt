package com.remoteplay.client.ui

import android.view.SurfaceView
import android.util.Log
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.TextButton
import androidx.compose.material3.MaterialTheme
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
import com.remoteplay.client.VideoDimensions
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.text.input.KeyboardType

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
    var videoDimensions by remember { mutableStateOf(VideoDimensions(1920, 1080)) }
    val mediaPaused by RemotePlayClient.mediaPaused.collectAsState()
    val pausePending by RemotePlayClient.mediaPausePending.collectAsState()
    var showStreamSettings by remember { mutableStateOf(false) }
    var fpsText by remember { mutableStateOf(RemotePlayClient.streamFps.toString()) }
    var bitrateText by remember { mutableStateOf(RemotePlayClient.streamBitrateKbps.toString()) }
    var keepAliveText by remember { mutableStateOf(RemotePlayClient.backgroundKeepAliveSeconds.toString()) }
    var settingsError by remember { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()

    if (showStreamSettings) {
        AlertDialog(onDismissRequest = { showStreamSettings = false },
            title = { Text("Stream settings") },
            text = { Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                OutlinedTextField(value = fpsText, onValueChange = { fpsText = it },
                    label = { Text("Frame rate (FPS)") }, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number))
                OutlinedTextField(value = bitrateText, onValueChange = { bitrateText = it },
                    label = { Text("Video bitrate (kbps)") }, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number))
                OutlinedTextField(value = keepAliveText, onValueChange = { keepAliveText = it },
                    label = { Text("Background keepalive (seconds)") }, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number))
                Text("0 keeps the connection. After the limit, disconnect to save power and reconnect on return.")
                settingsError?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            } },
            confirmButton = { TextButton(onClick = {
                val fps = fpsText.toIntOrNull()
                val bitrate = bitrateText.toIntOrNull()
                val keepAlive = keepAliveText.toIntOrNull()
                if (fps == null || bitrate == null || fps <= 0 || bitrate <= 0 || keepAlive == null || keepAlive !in 0..86400) {
                    settingsError = "Use positive FPS/kbps and 0–86400 keepalive seconds"
                } else scope.launch {
                    settingsError = withContext(Dispatchers.IO) { RemotePlayClient.updateStreamRates(fps, bitrate) }
                    if (settingsError == null) settingsError = RemotePlayClient.updateBackgroundKeepAlive(keepAlive)
                    if (settingsError == null) showStreamSettings = false
                }
            }) { Text("Apply") } },
            dismissButton = { TextButton(onClick = { showStreamSettings = false }) { Text("Cancel") } })
    }

    BoxWithConstraints(
        modifier = Modifier
            .fillMaxSize()
            .background(Color.Black)
            .safeDrawingPadding()
    ) {
        // Video and input share the actual visible decoded rectangle, excluding padding.
        AndroidView(
            modifier = Modifier
                .align(Alignment.Center)
                .width(minOf(maxWidth, maxHeight * videoDimensions.aspectRatio))
                .aspectRatio(videoDimensions.aspectRatio)
                .pointerInput(videoDimensions) {
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
                                val codec = MediaCodecPlayer(holder.surface) { dimensions ->
                                    post {
                                        if (active.get()) {
                                            videoDimensions = dimensions
                                            RemotePlayClient.setScreenBounds(dimensions.width, dimensions.height)
                                        }
                                    }
                                }
                                val audioPlayer = AudioOpusPlayer()
                                var audioEnabled = true
                                fun report(message: String, error: Exception) {
                                    Log.e("RemotePlay", message, error)
                                    post { if (active.get()) playbackError = message }
                                }
                                try {
                                    codec.start(1920, 1080)
                                    if (active.get()) RemotePlayClient.setViewerSurfaceReady(true)
                                    var waitingForKeyframe = true
                                    var pending: com.remoteplay.client.EncodedVideoFrame? = null
                                    var pendingSince = 0L
                                    var lastKeyframeRequest = System.nanoTime()
                                    var sampleTime = lastKeyframeRequest
                                    var sampleFrames = 0L
                                    var pauseHandled = false
                                    RemotePlayClient.requestKeyframe()
                                    while (active.get() && !Thread.currentThread().isInterrupted) {
                                        if (RemotePlayClient.mediaPaused.value) {
                                            if (!pauseHandled) {
                                                pending = null
                                                waitingForKeyframe = true
                                                codec.flush()
                                                audioPlayer.stop()
                                                pauseHandled = true
                                            }
                                            Thread.sleep(50)
                                            continue
                                        }
                                        val now = System.nanoTime()
                                        if (pauseHandled) {
                                            sampleTime = now
                                            sampleFrames = codec.outputFrames
                                            pauseHandled = false
                                        }
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
                            RemotePlayClient.setScreenBounds(videoDimensions.width, videoDimensions.height)
                        }
                        override fun surfaceDestroyed(holder: android.view.SurfaceHolder) {
                            running.set(false)
                            RemotePlayClient.setViewerSurfaceReady(false)
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
            TextButton(onClick = {
                RemotePlayClient.setManualMediaPaused(!RemotePlayClient.manualMediaPaused)
            }) { Text(if (RemotePlayClient.manualMediaPaused) "Resume" else "Pause") }
            TextButton(onClick = {
                fpsText = RemotePlayClient.streamFps.toString()
                bitrateText = RemotePlayClient.streamBitrateKbps.toString()
                settingsError = null
                showStreamSettings = true
            }) { Text("Stream settings") }
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
                            else if (pausePending) "Waiting for host…"
                            else if (mediaPaused) "Paused · connected"
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
