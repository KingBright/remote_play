package `fun`.hackerlife.weavelo

import android.content.Context
import android.content.Intent
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import org.json.JSONObject
import java.io.File

data class FileReceiverState(val active: Boolean = false, val starting: Boolean = false, val message: String = "")

/** An explicit receive switch; it neither requests capture nor survives force-stop. */
object FileReceiverClient {
    private val mutex = Mutex()
    private var owner: Any? = null
    private val mutable = MutableStateFlow(FileReceiverState())
    val state = mutable.asStateFlow()
    val ownsNetwork: Boolean get() = mutable.value.active || mutable.value.starting

    fun start(context: Context) {
        if (ownsNetwork) return
        mutable.value = FileReceiverState(starting = true, message = "Starting file reception...")
        try {
            context.startForegroundService(Intent(context, FileReceiverService::class.java))
        } catch (error: RuntimeException) {
            mutable.value = FileReceiverState(message = error.message ?: "Could not start file reception")
        }
    }
    fun stop(context: Context) {
        context.stopService(Intent(context, FileReceiverService::class.java))
    }
    internal suspend fun acquire(context: Context, token: Any) = mutex.withLock {
        check(WeaveloClient.nativeAvailable) { "Native connection is unavailable" }
        val directory = File(context.filesDir, "received")
        check(directory.isDirectory || directory.mkdirs()) { "Receive directory is unavailable" }
        PublisherClient.action(5, JSONObject().put("bind", "0.0.0.0:5001")
            .put("directory", directory.absolutePath).toString())
        owner = token
        mutable.value = FileReceiverState(active = true,
            message = "File reception enabled for this device network. Screen sharing is controlled separately.")
    }
    internal suspend fun checkHealth(token: Any) = mutex.withLock {
        if (owner === token) check(JSONObject(PublisherClient.action(7)).optBoolean("receiving")) {
            "File reception stopped; enable it again to reconnect"
        }
    }
    internal suspend fun release(token: Any, failure: String?) = mutex.withLock {
        // Delayed cleanup from an old Service instance must not stop a new owner.
        if (owner != null && owner !== token) return@withLock
        var message = failure ?: "Standalone receiver stopped. An active screen-sharing session can still receive files."
        if (owner === token) {
            try { PublisherClient.action(6) } catch (error: Exception) {
                message = error.message ?: "Could not stop file reception"
            }
        }
        owner = null
        mutable.value = FileReceiverState(message = message)
    }
}
