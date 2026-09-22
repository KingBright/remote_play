package com.remoteplay.client

import android.content.Context
import android.content.Intent
import android.os.SystemClock
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.util.concurrent.Executors

// Each source has its own subscription; audio owners may be shared by same-app windows.
data class WorkspaceSource(val source: Any, val title: String, val width: Int, val height: Int)
data class WorkspacePane(val id: Int, val source: WorkspaceSource, val fps: Int, val kbps: Int,
    val width: Int = 1280, val height: Int = 720, val audioOwner: Int? = null, val input: Boolean = false)
data class WorkspaceState(val connected: Boolean = false, val clipboard: Boolean = false, val maxSubscriptions: Int = 4,
    val files: Boolean = false, val clipboardAvailable: Boolean = false, val sources: List<WorkspaceSource> = emptyList(),
    val panes: List<WorkspacePane> = emptyList(), val status: String = "Connecting…", val grid: Boolean = true, val selected: Int = 0)

object WorkspaceClient {
    private external fun nativeAction(operation: Int, payload: String): String
    external fun nativeMedia(id: Int, audio: Boolean): ByteArray?
    private val scope = CoroutineScope(SupervisorJob() + Executors.newSingleThreadExecutor().asCoroutineDispatcher())
    private val mutable = MutableStateFlow(WorkspaceState())
    val state = mutable.asStateFlow()
    @Volatile var active = false
        private set
    @Volatile private var background = false
    private var backgroundAt = 0L
    private var context: Context? = null
    private var endpoint = ""
    private var nextId = 256
    private var revision = 0L
    private var polling: Job? = null
    private var asleep = false
    @Volatile var retainingConnection = false
        private set
    private var transfersActive = false
    private var lastFileAction = 0L
    private var pendingClipboard: JSONObject? = null

    private fun action(op: Int, payload: String = ""): String {
        val result = nativeAction(op, payload)
        if (result.startsWith("{\"error\"")) throw IllegalStateException(JSONObject(result).getString("error"))
        return result
    }
    private fun command(name: String, value: JSONObject) = action(2, JSONObject().put(name, value).toString())
    private fun request(p: WorkspacePane) = JSONObject().put("id", p.id).put("source", p.source.source)
        .put("width", p.width).put("height", p.height).put("fps", p.fps).put("bitrate_kbps", p.kbps).put("audio", true)
    private fun guarded(block: () -> Unit) = scope.launch {
        runCatching(block).onFailure { mutable.value = mutable.value.copy(status = it.message ?: "Connection failed") }
    }

    fun open(context: Context, endpoint: String) = guarded {
        closeNative()
        this.context = context.applicationContext
        this.endpoint = endpoint
        mutable.value = WorkspaceState()
        nextId = 256; revision = 0; asleep = false; background = false; transfersActive = false
        connectNative()
        active = true
        context.startForegroundService(Intent(context, SessionConnectionService::class.java))
        polling = scope.launch {
            while (isActive && active) {
                if (!asleep) runCatching { drainEvents() }.onFailure { mutable.value = mutable.value.copy(status = it.message ?: "Connection error") }
                if (background && !asleep) {
                    val seconds = BackgroundConnectionPolicy.currentSeconds(context)
                    if (seconds > 0 && !transfersActive && SystemClock.elapsedRealtime() - lastFileAction >= 5_000 && SystemClock.elapsedRealtime() - backgroundAt >= seconds * 1000L) {
                        action(1); asleep = true; retainingConnection = false
                        mutable.value = mutable.value.copy(connected = false, status = "Connection sleeping to save power")
                    }
                }
                delay(if (background) 1000 else 50)
            }
        }
    }
    private fun connectNative() {
        val directory = File(checkNotNull(context).filesDir, "received").apply { mkdirs() }
        action(0, JSONObject().put("endpoint", endpoint).put("directory", directory.absolutePath).toString())
        retainingConnection = true
        mutable.value = mutable.value.copy(connected = true, status = "Connected")
    }
    private fun closeNative() { polling?.cancel(); polling = null; if (active) action(1); active = false; retainingConnection = false }
    fun close() = guarded { closeNative(); mutable.value = WorkspaceState(status = "Disconnected") }
    fun reconnect() = guarded {
        connectNative(); asleep = false
        checkNotNull(context).startForegroundService(Intent(context, SessionConnectionService::class.java))
        mutable.value.panes.forEach { command("Subscribe", request(it)) }
        if (mutable.value.clipboard) command("SetClipboard", JSONObject().put("enabled", true))
        activity()
    }
    fun backgrounded(value: Boolean) = guarded {
        if (!active || background == value) return@guarded
        background = value
        if (value) backgroundAt = SystemClock.elapsedRealtime()
        if (!value && asleep) {
            connectNative(); asleep = false
            checkNotNull(context).startForegroundService(Intent(context, SessionConnectionService::class.java))
            mutable.value.panes.forEach { command("Subscribe", request(it)) }
            if (mutable.value.clipboard) command("SetClipboard", JSONObject().put("enabled", true))
        }
        if (!value) pendingClipboard?.let { applyClipboard(it); pendingClipboard = null }
        activity()
    }
    fun add(source: WorkspaceSource) = guarded {
        // Conservative default for mobile hardware; source count is distinct from visible decoder count.
        if (mutable.value.panes.size >= minOf(4, mutable.value.maxSubscriptions)) { mutable.value = mutable.value.copy(status = "This connection supports ${minOf(4, mutable.value.maxSubscriptions)} window(s)"); return@guarded }
        val prefs = checkNotNull(context).getSharedPreferences("stream", Context.MODE_PRIVATE)
        val pane = WorkspacePane(nextId, source, prefs.getInt("fps", 60), prefs.getInt("bitrate_kbps", 20_000))
        nextId += 256
        mutable.value = mutable.value.copy(panes = mutable.value.panes + pane, selected = pane.id)
        command("Subscribe", request(pane)); activity()
    }
    fun remove(id: Int) = guarded {
        command("Unsubscribe", JSONObject().put("id", id))
        val panes = mutable.value.panes.filter { it.id != id }
        mutable.value = mutable.value.copy(panes = panes, selected = panes.firstOrNull()?.id ?: 0)
        activity()
    }
    fun layout(grid: Boolean, selected: Int = mutable.value.selected) = guarded {
        mutable.value = mutable.value.copy(grid = grid, selected = selected); activity()
    }
    private fun activity() {
        if (asleep) return
        val s = mutable.value
        s.panes.forEach { pane ->
            val visible = !background && (s.grid || s.selected == pane.id)
            command("SetActivity", JSONObject().put("id", pane.id).put("revision", ++revision).put("video", visible).put("audio", visible))
        }
    }
    fun settings(id: Int, width: Int? = null, height: Int? = null, fps: Int? = null, kbps: Int? = null) = guarded {
        val pane = mutable.value.panes.find { it.id == id } ?: return@guarded
        val updated = pane.copy(width = width ?: pane.width, height = height ?: pane.height, fps = fps ?: pane.fps, kbps = kbps ?: pane.kbps)
        require(updated.fps > 0 && updated.kbps > 0) { "Frame rate and bitrate must be positive" }
        if (updated == pane) return@guarded
        action(3, request(updated).toString())
        mutable.value = mutable.value.copy(panes = mutable.value.panes.map { if (it.id == id) updated else it })
    }
    fun sendFile(file: File) = guarded { lastFileAction = SystemClock.elapsedRealtime(); action(4, file.absolutePath) }
    fun enableClipboard(enabled: Boolean) = guarded {
        command("SetClipboard", JSONObject().put("enabled", enabled))
        mutable.value = mutable.value.copy(clipboard = enabled)
        if (!enabled) pendingClipboard = null
    }
    fun sendClipboard(value: JSONObject) = guarded { lastFileAction = SystemClock.elapsedRealtime(); action(7, value.toString()) }
    private fun applyClipboard(value: JSONObject) {
        if (!mutable.value.clipboard) return
        if (background) { pendingClipboard = value; return }
        val ctx = checkNotNull(context)
        android.os.Handler(android.os.Looper.getMainLooper()).post {
            if (background || !mutable.value.clipboard) return@post
            runCatching {
                val clipboard = ctx.getSystemService(android.content.ClipboardManager::class.java)
                val clip = when {
                    value.has("text") -> android.content.ClipData.newPlainText("RemotePlay", value.getString("text"))
                    value.has("image") -> android.content.ClipData.newUri(ctx.contentResolver, "RemotePlay image", androidx.core.content.FileProvider.getUriForFile(ctx, "${ctx.packageName}.files", File(value.getString("image"))))
                    value.has("clipboard_files") -> {
                        val files = value.getJSONArray("clipboard_files")
                        if (files.length() == 0) return@post
                        fun uri(index: Int) = androidx.core.content.FileProvider.getUriForFile(ctx, "${ctx.packageName}.files", File(files.getString(index)))
                        android.content.ClipData.newUri(ctx.contentResolver, "RemotePlay files", uri(0)).apply { for (i in 1 until files.length()) addItem(android.content.ClipData.Item(uri(i))) }
                    }
                    else -> return@post
                }
                clipboard.setPrimaryClip(clip)
            }.onFailure { mutable.value = mutable.value.copy(status = it.message ?: "Clipboard unavailable") }
        }
    }
    fun keyframe(id: Int) = guarded { action(6, id.toString()) }
    fun touch(id: Int, event: JSONObject) = guarded { command("Input", JSONObject().put("id", id).put("event", JSONObject().put("Touch", event))) }
    private fun drainEvents() {
        val events = JSONArray(action(5))
        for (i in 0 until events.length()) {
            val event = events.getJSONObject(i)
            if (event.has("transfers_active")) transfersActive = event.getBoolean("transfers_active")
            val control = event.optJSONObject("control")
            control?.optJSONObject("Opened")?.let { mutable.value = mutable.value.copy(maxSubscriptions = it.getInt("max_subscriptions"), files = it.getBoolean("files"), clipboardAvailable = it.getBoolean("clipboard")) }
            control?.optJSONObject("Closed")?.let { action(1); asleep = true; retainingConnection = false; mutable.value = mutable.value.copy(connected = false, status = it.getString("reason")) }
            control?.optJSONObject("ClipboardState")?.let { mutable.value = mutable.value.copy(clipboard = it.getBoolean("enabled")) }
            event.optJSONObject("clipboard")?.let { applyClipboard(it) }
            event.optJSONObject("detail")?.takeIf { it.has("clipboard_files") }?.let { applyClipboard(it) }
            control?.optJSONObject("Sources")?.let { response ->
                val sources = response.getJSONArray("sources")
                mutable.value = mutable.value.copy(sources = (0 until sources.length()).map { index ->
                    val s = sources.getJSONObject(index)
                    WorkspaceSource(s.get("source"), s.getString("title"), s.getInt("width"), s.getInt("height"))
                })
            }
            control?.optJSONObject("Subscribed")?.let { subscribed ->
                mutable.value = mutable.value.copy(panes = mutable.value.panes.map {
                    if (it.id == subscribed.getInt("id")) it.copy(audioOwner = if (subscribed.isNull("audio_owner")) null else subscribed.getInt("audio_owner"), input = subscribed.getBoolean("supports_input")) else it
                })
                activity()
            }
            control?.optJSONObject("Error")?.let { mutable.value = mutable.value.copy(status = it.getString("reason")) }
            if (event.has("file")) mutable.value = mutable.value.copy(status = event.getString("file").take(220))
        }
    }
}
