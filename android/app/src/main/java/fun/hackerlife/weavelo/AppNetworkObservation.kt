package `fun`.hackerlife.weavelo

import android.app.ActivityManager
import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.os.Build
import android.os.Process
import android.os.SystemClock
import java.util.concurrent.atomic.AtomicReference
import kotlinx.serialization.json.*

data class AppNetworkStatus(
    val uid: Int = -1, val activeNetwork: Long? = null, val boundNetwork: Long? = null,
    val callbackNetwork: Long? = null, val blocked: Boolean? = null,
    val transports: List<String> = emptyList(), val internet: Boolean? = null,
    val validated: Boolean? = null, val dnsServers: Int = 0, val privateDns: Boolean? = null,
    val proxyConfigured: Boolean? = null, val dataSaver: String = "unknown",
    val backgroundRestricted: Boolean? = null, val availableEvents: Int = 0, val lostEvents: Int = 0,
    val observedElapsedMs: Long = 0, val observationError: Boolean = false,
    val platformProbe: PlatformRelayProbeResult = PlatformRelayProbeResult(),
    val httpsControl: PlatformRelayProbeResult = PlatformRelayProbeResult()
) {
    fun publicEvidenceJson(): String = buildJsonObject {
        put("source", "application_uid"); put("uid", uid)
        put("active_network", activeNetwork?.let(::JsonPrimitive) ?: JsonNull)
        put("bound_process_network", boundNetwork?.let(::JsonPrimitive) ?: JsonNull)
        put("callback_network", callbackNetwork?.let(::JsonPrimitive) ?: JsonNull)
        put("callback_blocked", blocked?.let(::JsonPrimitive) ?: JsonNull)
        put("transports", JsonArray(transports.map(::JsonPrimitive)))
        put("internet", internet?.let(::JsonPrimitive) ?: JsonNull)
        put("validated", validated?.let(::JsonPrimitive) ?: JsonNull)
        put("dns_server_count", dnsServers); put("private_dns", privateDns?.let(::JsonPrimitive) ?: JsonNull)
        put("proxy_configured", proxyConfigured?.let(::JsonPrimitive) ?: JsonNull)
        put("data_saver", dataSaver); put("background_restricted", backgroundRestricted?.let(::JsonPrimitive) ?: JsonNull)
        put("available_events", availableEvents); put("lost_events", lostEvents)
        put("observed_elapsed_ms", observedElapsedMs); put("observation_error", observationError)
        put("platform_probe", Json.parseToJsonElement(platformProbe.publicEvidenceJson()))
        put("https_control_probe", Json.parseToJsonElement(httpsControl.publicEvidenceJson()))
    }.toString()
}

/** One read-only callback for the application lifetime. Never bind or request a network. */
class AppNetworkObservation(private val context: Context) {
    private val manager = context.getSystemService(ConnectivityManager::class.java)
    private val probe = PlatformRelayProbe()
    private val httpsControl = PlatformRelayProbe(ProbeDestination.HTTPS_CONTROL)
    private data class CallbackState(val network: Network? = null, val blocked: Boolean? = null, val available: Int = 0, val lost: Int = 0)
    private val state = AtomicReference(CallbackState())
    private var registered = false
    private val callback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) {
            state.updateAndGet { it.copy(network = network, blocked = null, available = it.available + 1) }
        }
        override fun onLost(network: Network) {
            state.updateAndGet { if (it.network == network) it.copy(network = null, blocked = null, lost = it.lost + 1) else it }
        }
        override fun onBlockedStatusChanged(network: Network, blocked: Boolean) {
            state.updateAndGet { if (it.network == network) it.copy(blocked = blocked) else it }
        }
    }
    @Synchronized fun start() {
        if (!registered) {
            manager.registerDefaultNetworkCallback(callback)
            registered = true
            probe.start()
            httpsControl.start()
        }
    }
    fun runProbe() { probe.start(); httpsControl.start() }
    fun read(): AppNetworkStatus {
        val callbacks = state.get()
        return try {
            val active = manager.activeNetwork
            val capabilities = active?.let(manager::getNetworkCapabilities)
            val properties = active?.let(manager::getLinkProperties)
            val transports = listOf("vpn" to NetworkCapabilities.TRANSPORT_VPN, "wifi" to NetworkCapabilities.TRANSPORT_WIFI,
                "cellular" to NetworkCapabilities.TRANSPORT_CELLULAR, "ethernet" to NetworkCapabilities.TRANSPORT_ETHERNET)
                .filter { capabilities?.hasTransport(it.second) == true }.map { it.first }
            AppNetworkStatus(
                uid = Process.myUid(), activeNetwork = active?.toString()?.toLongOrNull(),
                boundNetwork = manager.boundNetworkForProcess?.toString()?.toLongOrNull(),
                callbackNetwork = callbacks.network?.toString()?.toLongOrNull(), blocked = callbacks.blocked, transports = transports,
                internet = capabilities?.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET),
                validated = capabilities?.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED),
                dnsServers = properties?.dnsServers?.size ?: 0,
                privateDns = if (Build.VERSION.SDK_INT >= 28) properties?.isPrivateDnsActive else null,
                proxyConfigured = manager.defaultProxy != null,
                dataSaver = when (manager.restrictBackgroundStatus) {
                    ConnectivityManager.RESTRICT_BACKGROUND_STATUS_DISABLED -> "disabled"
                    ConnectivityManager.RESTRICT_BACKGROUND_STATUS_ENABLED -> "enabled"
                    ConnectivityManager.RESTRICT_BACKGROUND_STATUS_WHITELISTED -> "whitelisted"
                    else -> "unknown"
                },
                backgroundRestricted = if (Build.VERSION.SDK_INT >= 28) context.getSystemService(ActivityManager::class.java).isBackgroundRestricted else null,
                availableEvents = callbacks.available, lostEvents = callbacks.lost,
                observedElapsedMs = SystemClock.elapsedRealtime(), platformProbe = probe.result.get(), httpsControl = httpsControl.result.get())
        } catch (_: Exception) {
            AppNetworkStatus(uid = Process.myUid(), observationError = true, platformProbe = probe.result.get(), httpsControl = httpsControl.result.get())
        }
    }
}

fun publicConnectionEvidence(network: AppNetworkStatus, native: DiscoveryDiagnostics): String = buildJsonObject {
    put("android_network", Json.parseToJsonElement(network.publicEvidenceJson()))
    put("native_runtime", Json.parseToJsonElement(native.publicEvidenceJson()))
}.toString()
