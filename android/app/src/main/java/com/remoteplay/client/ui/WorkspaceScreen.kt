package com.remoteplay.client.ui

import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.MotionEvent
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import com.remoteplay.client.*
import kotlinx.coroutines.*
import org.json.JSONObject
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.atomic.AtomicBoolean

@Composable
fun WorkspaceScreen(endpoint: String, onClose: () -> Unit) {
    val context = LocalContext.current
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    val state by WorkspaceClient.state.collectAsState()
    var foreground by remember { mutableStateOf(lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)) }
    var localError by remember { mutableStateOf<String?>(null) }
    var receivedFiles by remember { mutableStateOf(false) }
    var backgroundSettings by remember { mutableStateOf(false) }
    val preferences = remember { context.getSharedPreferences("stream", android.content.Context.MODE_PRIVATE) }
    var keepAlive by remember { mutableStateOf(preferences.getInt("background_keepalive_seconds", 300).toString()) }
    var adaptive by remember { mutableStateOf(preferences.getBoolean("adaptive_background", true)) }
    val scope = rememberCoroutineScope()
    var selectedSettings by remember { mutableStateOf<WorkspacePane?>(null) }
    val volumes = remember { mutableStateMapOf<Int, Float>() }
    if (backgroundSettings) AlertDialog(onDismissRequest = { backgroundSettings = false }, title = { Text("Background connection") },
        text = { Column {
            OutlinedTextField(keepAlive, { keepAlive = it }, label = { Text("Idle seconds (0 = always keep)") },
                isError = keepAlive.toIntOrNull()?.let { it !in 0..86400 } ?: true)
            Row(verticalAlignment = Alignment.CenterVertically) {
                Checkbox(adaptive, { adaptive = it }); Text("Adjust for battery and mobile data")
            }
            Text("Media pauses immediately. Idle connections sleep sooner on battery saver (30s), low battery (60s), or metered data (120s). File transfers finish first. Reopen to reconnect.")
        } }, confirmButton = { TextButton(onClick = {
            val seconds = keepAlive.toIntOrNull()
            if (seconds != null && seconds in 0..86400) {
                RemotePlayClient.updateBackgroundKeepAlive(seconds)
                preferences.edit().putBoolean("adaptive_background", adaptive).apply()
                backgroundSettings = false
            }
        }) { Text("Save") } })
    DisposableEffect(endpoint) {
        WorkspaceClient.open(context, endpoint)
        val observer = LifecycleEventObserver { _, _ -> foreground = lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED) }
        lifecycle.addObserver(observer)
        onDispose { lifecycle.removeObserver(observer); WorkspaceClient.close() }
    }
    val filePicker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) scope.launch(Dispatchers.IO) {
            runCatching {
                val name = context.contentResolver.query(uri, arrayOf(android.provider.OpenableColumns.DISPLAY_NAME), null, null, null)?.use {
                    if (it.moveToFirst()) it.getString(0) else null
                }?.substringAfterLast('/')?.substringAfterLast('\\')?.takeIf { it != "." && it != ".." && it.isNotBlank() } ?: "shared-file"
                val directory = File(context.cacheDir, "outgoing/${java.util.UUID.randomUUID()}").apply { mkdirs() }
                val file = File(directory, name)
                checkNotNull(context.contentResolver.openInputStream(uri)).use { input -> file.outputStream().use { output -> input.copyTo(output, 64 * 1024) } }
                WorkspaceClient.sendFile(file)
            }.onFailure { withContext(Dispatchers.Main) { localError = it.message } }
        }
    }
    MaterialTheme {
        Column(Modifier.fillMaxSize().background(Color(0xff101318)).safeDrawingPadding().padding(8.dp)) {
            Row(Modifier.horizontalScroll(rememberScrollState()), verticalAlignment = Alignment.CenterVertically) {
                TextButton(onClick = onClose) { Text("Disconnect") }
                if (!state.connected) TextButton(onClick = { WorkspaceClient.reconnect() }) { Text("Reconnect") }
                TextButton(onClick = { WorkspaceClient.layout(!state.grid) }) { Text(if (state.grid) "Use tabs" else "Show together") }
                TextButton(onClick = { filePicker.launch(arrayOf("*/*")) }, enabled = state.connected && state.files) { Text("Send file") }
                TextButton(onClick = { receivedFiles = true }) { Text("Received files") }
                TextButton(onClick = { backgroundSettings = true }) { Text("Background") }
                TextButton(onClick = { WorkspaceClient.enableClipboard(!state.clipboard) }, enabled = state.connected && state.clipboardAvailable) { Text(if (state.clipboard) "Clipboard ✓" else "Clipboard") }
                TextButton(onClick = {
                    // Android only allows reading the clipboard while this app has focus.
                    val clip = context.getSystemService(android.content.ClipboardManager::class.java).primaryClip
                    if (clip != null) scope.launch(Dispatchers.IO) {
                        runCatching {
                            val uris = (0 until clip.itemCount).mapNotNull { clip.getItemAt(it).uri }
                            require(uris.size <= 16) { "Send at most 16 clipboard files at a time" }
                            if (uris.isEmpty()) {
                                val text = clip.getItemAt(0).text?.toString() ?: error("Clipboard has no transferable content")
                                require(text.toByteArray().size <= 1024 * 1024) { "Clipboard text is too large" }
                                WorkspaceClient.sendClipboard(JSONObject().put("text", text))
                            } else {
                                val directory = File(context.cacheDir, "outgoing/${java.util.UUID.randomUUID()}").apply { mkdirs() }
                                val files = uris.take(16).mapIndexed { index, uri ->
                                    val name = context.contentResolver.query(uri, arrayOf(android.provider.OpenableColumns.DISPLAY_NAME), null, null, null)?.use {
                                        if (it.moveToFirst()) it.getString(0) else null
                                    }?.substringAfterLast('/')?.substringAfterLast('\\')?.replace(':', '_')
                                        ?.takeIf { it != "." && it != ".." && it.isNotBlank() } ?: "item-$index"
                                    val folder = File(directory, index.toString()).apply { mkdirs() }
                                    val file = File(folder, name)
                                    checkNotNull(context.contentResolver.openInputStream(uri)).use { input -> file.outputStream().use { input.copyTo(it, 64 * 1024) } }
                                    file
                                }
                                if (uris.size == 1 && context.contentResolver.getType(uris[0])?.startsWith("image/") == true) {
                                    val bounds = android.graphics.BitmapFactory.Options().apply { inJustDecodeBounds = true }
                                    android.graphics.BitmapFactory.decodeFile(files[0].absolutePath, bounds)
                                    require(bounds.outWidth > 0 && bounds.outHeight > 0 && bounds.outWidth.toLong() * bounds.outHeight <= 16_777_216) { "Clipboard image is too large or unsupported" }
                                    val bitmap = checkNotNull(android.graphics.BitmapFactory.decodeFile(files[0].absolutePath))
                                    val png = File(directory, "clipboard.png")
                                    try { png.outputStream().use { check(bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)) } } finally { bitmap.recycle() }
                                    WorkspaceClient.sendClipboard(JSONObject().put("image", png.absolutePath).put("mime", "image/png"))
                                } else WorkspaceClient.sendClipboard(JSONObject().put("files", org.json.JSONArray(files.map { it.absolutePath })))
                            }
                        }.onFailure { withContext(Dispatchers.Main) { localError = it.message } }
                    }
                }, enabled = state.connected && state.clipboard) { Text("Send clipboard") }
            }
            Text(localError ?: state.status, color = Color.LightGray, maxLines = 2)
            Row(Modifier.horizontalScroll(rememberScrollState())) {
                state.sources.forEach { source -> TextButton(onClick = { WorkspaceClient.add(source) }, enabled = state.connected) { Text("+ ${source.title}", maxLines = 1) } }
            }
            if (!state.grid) Row(Modifier.horizontalScroll(rememberScrollState())) {
                state.panes.forEach { pane -> TextButton(onClick = { WorkspaceClient.layout(false, pane.id) }) { Text(pane.source.title) } }
            }
            val visible = if (state.grid) state.panes else state.panes.filter { it.id == state.selected }
            val rows = visible.chunked(if (state.grid && visible.size > 1) 2 else 1)
            rows.forEach { row ->
                Row(Modifier.weight(1f).fillMaxWidth()) {
                    row.forEach { pane ->
                        key(pane.id) {
                            Column(Modifier.weight(1f).fillMaxHeight().padding(3.dp)) {
                                Row(Modifier.fillMaxWidth()) {
                                    TextButton(onClick = { selectedSettings = pane }, modifier = Modifier.weight(1f)) { Text("${pane.source.title} · ${pane.fps} fps", maxLines = 1, overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis) }
                                    TextButton(onClick = { WorkspaceClient.remove(pane.id) }, modifier = Modifier.width(48.dp), contentPadding = PaddingValues(0.dp)) { Text("×") }
                                }
                                pane.audioOwner?.let { owner ->
                                    Slider(value = volumes[owner] ?: 1f, onValueChange = { volumes[owner] = it }, modifier = Modifier.height(24.dp))
                                }
                                Box(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
                                    if (foreground && state.connected) WorkspaceVideo(pane, onError = { localError = it })
                                }
                            }
                        }
                    }
                }
            }
            if (state.panes.isEmpty()) Text("Choose a display or application window above. Files work without opening a video.", color = Color.LightGray)
            if (foreground && state.connected) visible.mapNotNull { it.audioOwner }.distinct().forEach { owner ->
                key(owner) { WorkspaceAudio(owner, volumes[owner] ?: 1f, onError = { localError = it }) }
            }
        }
        selectedSettings?.let { pane ->
            var fps by remember(pane.id) { mutableStateOf(pane.fps.toString()) }
            var kbps by remember(pane.id) { mutableStateOf(pane.kbps.toString()) }
            AlertDialog(onDismissRequest = { selectedSettings = null }, title = { Text("Stream settings") },
                text = { Column { OutlinedTextField(fps, { fps = it }, label = { Text("Frames per second") }); OutlinedTextField(kbps, { kbps = it }, label = { Text("Bitrate (kbps)") }) } },
                confirmButton = { TextButton(onClick = {
                    val f = fps.toIntOrNull(); val k = kbps.toIntOrNull()
                    if (f != null && k != null && f > 0 && k > 0) { WorkspaceClient.settings(pane.id, fps = f, kbps = k); selectedSettings = null }
                }) { Text("Apply") } })
        }
        if (receivedFiles) ReceivedFilesDialog { receivedFiles = false }
    }
}

@Composable
private fun WorkspaceAudio(owner: Int, volume: Float, onError: (String) -> Unit) {
    val player = remember(owner) { AudioOpusPlayer() }
    SideEffect { player.setVolume(volume) }
    DisposableEffect(player) { onDispose { player.stop() } }
    LaunchedEffect(owner) {
        withContext(Dispatchers.IO) {
            try {
                while (isActive) {
                    repeat(8) { WorkspaceClient.nativeMedia(owner + 2, true)?.let { player.feed(it) } }
                    player.drain(); delay(5)
                }
            } catch (e: Exception) { if (e !is CancellationException) withContext(Dispatchers.Main) { onError(e.message ?: "Audio failed") } }
            finally { player.stop() }
        }
    }
}

@Composable
private fun WorkspaceVideo(pane: WorkspacePane, onError: (String) -> Unit) {
    var ratio by remember(pane.id) { mutableFloatStateOf(if (pane.source.height > 0) pane.source.width.toFloat() / pane.source.height else 16f / 9f) }
    var measured by remember { mutableStateOf(androidx.compose.ui.unit.IntSize.Zero) }
    val currentError by rememberUpdatedState(onError)
    val supportsInput by rememberUpdatedState(pane.input)
    LaunchedEffect(measured) {
        delay(250)
        if (measured.width > 1 && measured.height > 1) WorkspaceClient.settings(pane.id,
            width = ((measured.width + 31) / 32 * 32).coerceIn(32, 4096), height = ((measured.height + 31) / 32 * 32).coerceIn(32, 4096))
    }
    BoxWithConstraints(Modifier.fillMaxSize().onSizeChanged { measured = it }, contentAlignment = Alignment.Center) {
        val fit = if (maxWidth / maxHeight > ratio) Modifier.fillMaxHeight().aspectRatio(ratio, true) else Modifier.fillMaxWidth().aspectRatio(ratio)
        AndroidView(modifier = fit, factory = { context ->
            SurfaceView(context).apply {
                holder.addCallback(object : SurfaceHolder.Callback {
                    private val active = AtomicBoolean(false)
                    private var worker: Thread? = null
                    override fun surfaceCreated(holder: SurfaceHolder) {
                        active.set(true)
                        worker = Thread({
                            val codec = MediaCodecPlayer(holder.surface) { dimensions -> post { ratio = dimensions.width.toFloat() / dimensions.height } }
                            try {
                                codec.start(pane.width, pane.height)
                                WorkspaceClient.keyframe(pane.id)
                                var waiting = true
                                var pending: ByteArray? = null
                                var pendingSince = 0L
                                var lastRequest = System.nanoTime()
                                var reportedFirstFrame = false
                                while (active.get()) {
                                    if (pending == null) {
                                        pending = WorkspaceClient.nativeMedia(pane.id, false)
                                        pendingSince = System.nanoTime()
                                    }
                                    val bytes = pending
                                    if (bytes != null && bytes.size > 9) {
                                        val keyframe = bytes[0].toInt() != 0
                                        val pts = ByteBuffer.wrap(bytes, 1, 8).order(ByteOrder.LITTLE_ENDIAN).long
                                        if (waiting && !keyframe) pending = null
                                        else if (codec.feedNalu(bytes, keyframe, pts, 9)) { pending = null; waiting = false }
                                    }
                                    if (pending != null && System.nanoTime() - pendingSince > 250_000_000) {
                                        pending = null; waiting = true
                                        codec.flush()
                                        WorkspaceClient.keyframe(pane.id)
                                        lastRequest = System.nanoTime()
                                    }
                                    codec.drain()
                                    if (!reportedFirstFrame && codec.outputFrames > 0) {
                                        android.util.Log.i("RemotePlayWorkspace", "First decoded frame for subscription ${pane.id}")
                                        reportedFirstFrame = true
                                    }
                                    if (waiting && System.nanoTime() - lastRequest > 1_000_000_000) { WorkspaceClient.keyframe(pane.id); lastRequest = System.nanoTime() }
                                    Thread.sleep(4)
                                }
                            } catch (e: Exception) { if (active.get()) post { currentError(e.message ?: "Video decoder failed") } }
                            finally { codec.stop() }
                        }, "workspace-video-${pane.id}").apply { start() }
                    }
                    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {}
                    override fun surfaceDestroyed(holder: SurfaceHolder) { active.set(false); worker?.interrupt(); worker?.join(500); worker = null }
                })
                setOnTouchListener { view, event ->
                    if (!supportsInput) false else {
                        val action = when (event.actionMasked) { MotionEvent.ACTION_DOWN -> "Down"; MotionEvent.ACTION_UP -> "Up"; MotionEvent.ACTION_CANCEL -> "Cancel"; else -> "Move" }
                        WorkspaceClient.touch(pane.id, JSONObject().put("action", action).put("pointer_id", 0)
                            .put("normalized_x", (event.x / view.width).coerceIn(0f, 1f)).put("normalized_y", (event.y / view.height).coerceIn(0f, 1f)).put("pressure", event.pressure))
                        if (event.actionMasked == MotionEvent.ACTION_UP) view.performClick()
                        true
                    }
                }
            }
        })
    }
}
