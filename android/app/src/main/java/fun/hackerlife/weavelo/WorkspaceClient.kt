package `fun`.hackerlife.weavelo

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
data class RemoteSharedFile(val id: Long, val name: String, val sizeBytes: Long)
data class WorkspaceState(val remoteFiles: List<RemoteSharedFile> = emptyList(), val sharedNext: Long? = null, val sharedBusy: Boolean = false, val sessionKey: String = "", val connectionGeneration: Long = 0, val connected: Boolean = false, val clipboard: Boolean = false, val maxSubscriptions: Int = 4,
    val files: Boolean = false, val clipboardAvailable: Boolean = false, val sources: List<WorkspaceSource> = emptyList(),
    val panes: List<WorkspacePane> = emptyList(), val status: String = "Connecting…", val grid: Boolean = true, val selected: Int = 0)

object WorkspaceClient {
    private external fun nativeAction(operation: Int, payload: String): String
    external fun nativeMedia(id: Int, audio: Boolean): ByteArray?
    private val dispatcher = Executors.newSingleThreadExecutor().asCoroutineDispatcher()
    private val scope = CoroutineScope(SupervisorJob() + dispatcher)
    private val ownership = WorkspaceSessionOwnership()
    private val pendingStarts = java.util.concurrent.atomic.AtomicInteger()
    val starting: Boolean get() = pendingStarts.get() > 0
    private val mutable = MutableStateFlow(WorkspaceState())
    val state = mutable.asStateFlow()
    @Volatile var active = false
        private set
    @Volatile private var background = false
    private var backgroundAt = 0L
    private var context: Context? = null
    private var endpoint = ""
    private var deviceId = ""
    private var connectionGeneration = 0L
    private val surfaceTokens = java.util.concurrent.atomic.AtomicLong()
    fun nextSurfaceToken(): Long = surfaceTokens.incrementAndGet()
    private var nextId = 256
    private var revision = 0L
    private var shareRequestId = 0L
    private var pendingShareList = 0L
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

    fun requestedSession(): WorkspaceSessionRequest? = ownership.request()
    fun sessionKeyFor(endpoint: String, deviceId: String): String = ownership.request()
        ?.takeIf { it.endpoint == endpoint && it.deviceId == deviceId }?.sessionKey
        ?: java.util.UUID.randomUUID().toString()

    fun resumeOnForeground(context: Context) {
        ownership.request()?.let { SessionConnectionService.openWorkspace(context, it) }
    }

    // Called only by the foreground Service. All native work remains serialized;
    // the polling Job is a child of the Service rather than the UI/global scope.
    fun openFromService(context: Context, ownerScope: CoroutineScope, owner: Any, request: WorkspaceSessionRequest, forceReconnect: Boolean = false) {
        pendingStarts.incrementAndGet()
        guarded {
            try {
                if (!ownerScope.isActive) return@guarded
                val previous = ownership.request()
                val sameSession = previous == request && mutable.value.sessionKey == request.sessionKey
                val reuse = ownership.attach(owner, request, active && !asleep && !forceReconnect)
                if (!reuse) {
                    closeNative()
                    this.context = context.applicationContext
                    endpoint = request.endpoint; deviceId = request.deviceId
                    mutable.value = if (sameSession) mutable.value.copy(connected = false, status = "Connecting…")
                        else WorkspaceState(sessionKey = request.sessionKey)
                    if (!sameSession) nextId = 256
                    revision = 0; asleep = false
                    background = WeaveloClient.backgrounded; transfersActive = false
                    if (background) backgroundAt = SystemClock.elapsedRealtime()
                    connectNative()
                    active = true
                    if (sameSession) {
                        mutable.value.panes.forEach { command("Subscribe", this.request(it)) }
                        if (mutable.value.clipboard) command("SetClipboard", JSONObject().put("enabled", true))
                        activity()
                    }
                }
                polling?.cancel()
                polling = ownerScope.launch(dispatcher) {
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
            } catch (error: Exception) {
                ownership.release(owner, preserveForForeground = true)
                throw error
            } finally { pendingStarts.decrementAndGet() }
        }
    }

    fun disconnectSession(sessionKey: String) = guarded {
        if (!ownership.disconnect(sessionKey)) return@guarded
        closeNative()
        mutable.value = WorkspaceState(status = "Disconnected")
    }

    fun closeFromService(owner: Any, preserveForForeground: Boolean) = guarded {
        if (!ownership.release(owner, preserveForForeground)) return@guarded
        closeNative()
        mutable.value = if (preserveForForeground) mutable.value.copy(connected = false,
            panes = mutable.value.panes.map { it.copy(input = false) }, status = "Connection sleeping; reopen to reconnect")
            else WorkspaceState(status = "Disconnected")
    }
    private fun connectNative() {
        val directory = File(checkNotNull(context).filesDir, "received").apply { mkdirs() }
        WeaveloClient.refreshDevices()
        val target = resolveNetworkEndpoint(deviceId, endpoint, WeaveloClient.devices.value)
        action(0, JSONObject().put("endpoint", target).put("directory", directory.absolutePath).toString())
        retainingConnection = true
        pendingShareList = 0L
        mutable.value = mutable.value.copy(connected = true, connectionGeneration = ++connectionGeneration, panes = mutable.value.panes.map { it.copy(input = false) }, remoteFiles = emptyList(), sharedNext = null, sharedBusy = false, status = "Connected")
    }
    private fun closeNative() { polling?.cancel(); polling = null; if (active) action(1); active = false; retainingConnection = false }
    fun reconnect() {
        ownership.request()?.let { SessionConnectionService.openWorkspace(checkNotNull(context), it, forceReconnect = true) }
    }
    fun backgrounded(value: Boolean) = guarded {
        if (!active || background == value) return@guarded
        background = value
        if (value) backgroundAt = SystemClock.elapsedRealtime()
        if (!value && asleep) {
            ownership.request()?.let { SessionConnectionService.openWorkspace(checkNotNull(context), it) }
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
    fun sendFile(file: File, expectedSessionKey: String) = guarded {
        check(active && mutable.value.connected && mutable.value.sessionKey == expectedSessionKey) {
            "The destination changed or disconnected. Select the file again for the intended device."
        }
        lastFileAction = SystemClock.elapsedRealtime()
        action(4, file.absolutePath)
    }
    fun publishSharedFile(file: File, expectedSessionKey: String) = guarded {
        check(active && mutable.value.connected && mutable.value.sessionKey == expectedSessionKey) { "The selected device session changed" }
        action(8, file.absolutePath)
        mutable.value = mutable.value.copy(status = "File shared with this network until cleared, network change, or app exit. Enable file reception on this phone for new incoming connections.")
    }
    fun clearSharedFiles() = guarded {
        action(10)
        mutable.value = mutable.value.copy(status = "Shared list cleared. Already accepted transfers may finish.")
    }
    fun listSharedFiles(afterId: Long = 0L) = guarded {
        check(active && mutable.value.connected && mutable.value.files) { "File connection is unavailable" }
        check(!mutable.value.sharedBusy) { "A shared-file list request is already pending" }
        val id = ++shareRequestId
        action(9, JSONObject().put("request_id", id).put("request", JSONObject().put("List", JSONObject().put("after_id", afterId))).toString())
        pendingShareList = id
        lastFileAction = SystemClock.elapsedRealtime()
        mutable.value = mutable.value.copy(sharedBusy = true, status = "Requesting shared files...")
    }
    fun fetchSharedFile(fileId: Long, expectedSessionKey: String) = guarded {
        check(active && mutable.value.connected && mutable.value.sessionKey == expectedSessionKey) { "The selected device session changed" }
        check(mutable.value.remoteFiles.any { it.id == fileId }) { "Refresh the remote shared list first" }
        action(9, JSONObject().put("request_id", ++shareRequestId).put("request", JSONObject().put("Fetch", JSONObject().put("file_id", fileId))).toString())
        lastFileAction = SystemClock.elapsedRealtime()
        mutable.value = mutable.value.copy(status = "File requested; waiting for transfer confirmation")
    }
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
                    value.has("text") -> android.content.ClipData.newPlainText("Weavelo", value.getString("text"))
                    value.has("image") -> android.content.ClipData.newUri(ctx.contentResolver, "Weavelo image", androidx.core.content.FileProvider.getUriForFile(ctx, "${ctx.packageName}.files", File(value.getString("image"))))
                    value.has("clipboard_files") -> {
                        val files = value.getJSONArray("clipboard_files")
                        if (files.length() == 0) return@post
                        fun uri(index: Int) = androidx.core.content.FileProvider.getUriForFile(ctx, "${ctx.packageName}.files", File(files.getString(index)))
                        android.content.ClipData.newUri(ctx.contentResolver, "Weavelo files", uri(0)).apply { for (i in 1 until files.length()) addItem(android.content.ClipData.Item(uri(i))) }
                    }
                    else -> return@post
                }
                clipboard.setPrimaryClip(clip)
            }.onFailure { mutable.value = mutable.value.copy(status = it.message ?: "Clipboard unavailable") }
        }
    }
    fun keyframe(id: Int) = guarded { action(6, id.toString()) }
    private fun currentView(id: Int, sessionKey: String, generation: Long, visible: Boolean): Boolean {
        val s = mutable.value
        return active && s.connected && s.sessionKey == sessionKey && s.connectionGeneration == generation &&
            s.panes.any { it.id == id } && (!visible || (!background && (s.grid || s.selected == id)))
    }
    fun surface(id: Int, sessionKey: String, generation: Long, token: Long, phase: String, eventTime: Long = SystemClock.uptimeMillis()) = guarded {
        if (!currentView(id, sessionKey, generation, phase == "begin" || phase == "ready" || phase == "alive")) return@guarded
        val currentPhase = if (phase == "alive" && SystemClock.uptimeMillis() - eventTime !in 0L..250L) "cancel" else phase
        action(11, JSONObject().put("id", id).put("token", token).put("phase", currentPhase).toString())
    }
    fun touch(id: Int, sessionKey: String, generation: Long, token: Long, eventTime: Long, event: JSONObject) = guarded {
        if (!currentView(id, sessionKey, generation, true)) return@guarded
        // Include time waiting on this serial executor; do not replay an old Android event as fresh input.
        if (SystemClock.uptimeMillis() - eventTime !in 0L..250L) {
            action(11, JSONObject().put("id", id).put("token", token).put("phase", "cancel").toString())
            return@guarded
        }
        action(12, JSONObject().put("id", id).put("token", token).put("event", JSONObject().put("Touch", event)).toString())
    }
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
            event.optJSONObject("detail")?.let { detail ->
                val response = detail.optJSONObject("shared_response")
                val id = detail.optLong("shared_request_id")
                response?.optJSONObject("Page")?.takeIf { id == pendingShareList }?.let { page ->
                    val entries = page.getJSONArray("entries")
                    val files = (0 until entries.length()).map { index ->
                        val item = entries.getJSONObject(index)
                        RemoteSharedFile(item.getLong("id"), item.getString("name"), item.getLong("size_bytes"))
                    }
                    pendingShareList = 0L
                    mutable.value = mutable.value.copy(remoteFiles = files, sharedNext = if (page.isNull("next_after_id")) null else page.getLong("next_after_id"), sharedBusy = false, status = if (files.isEmpty()) "No files explicitly shared by this device" else "Choose a shared file to retrieve")
                }
                response?.optJSONObject("Queued")?.let { mutable.value = mutable.value.copy(status = "Request accepted; delivery is not complete yet") }
                response?.optJSONObject("Rejected")?.let { rejected ->
                    if (id == pendingShareList) pendingShareList = 0L
                    mutable.value = mutable.value.copy(sharedBusy = pendingShareList != 0L, status = rejected.getString("message"))
                }
            }
        }
    }
}
