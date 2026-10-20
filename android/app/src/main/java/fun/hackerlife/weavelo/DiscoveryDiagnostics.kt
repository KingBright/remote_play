package `fun`.hackerlife.weavelo

import kotlinx.serialization.json.*

/** Public runtime observations only. Never parse invitations, capabilities or endpoint URLs. */
data class RelayDiagnostics(
    val state: String = "idle", val attempts: Long = 0, val registrationsSent: Long = 0,
    val pongsReceived: Long = 0, val dataFramesReceived: Long = 0, val error: String = "",
    val stage: String = "idle", val failedStage: String = "", val dnsResults: Long = 0,
    val tcpConnections: Long = 0, val tlsHandshakes: Long = 0, val websocketUpgrades: Long = 0,
    val tlsBytesSent: Long = 0, val tlsBytesReceived: Long = 0, val errno: Int? = null, val httpStatus: Int? = null,
    val dnsHashes: List<String> = emptyList(), val dnsBenchmarkCount: Long = 0
)
data class DiscoveryDiagnostics(
    val phase: String = "starting", val generation: Long = 0,
    val reloadRequested: Long = 0, val reloadApplied: Long = 0,
    val networkHash: String = "", val deviceHash: String = "", val peerHashes: List<String> = emptyList(),
    val routes: Long = 0, val peers: Long = 0, val p2p: String = "idle", val failure: String = "",
    val control: RelayDiagnostics = RelayDiagnostics(), val discovery: RelayDiagnostics = RelayDiagnostics()
) {
    val emptyMessage: String get() = when {
        reloadRequested > reloadApplied -> "Applying device network. Your pairing is retained."
        phase == "paused" -> "Discovery is paused while the app is in the background."
        phase == "retrying" -> "Connection interrupted. Retrying with your existing pairing. See Settings for connection status."
        phase == "starting" -> "Starting device discovery..."
        control.state == "retry_wait" || discovery.state == "retry_wait" -> "Reconnecting to the relay. Your pairing is retained; nearby discovery remains active."
        else -> "Looking for devices in your network. Keep the other device online. See Settings for connection status."
    }
    // Counters stay visible in Settings; log only transitions and first confirmed milestones.
    fun transitionKey(): List<Any> = listOf(phase, generation, reloadRequested, reloadApplied,
        networkHash, deviceHash, peerHashes, routes, peers, p2p, failure,
        control.state, control.attempts, control.pongsReceived > 0, control.dataFramesReceived > 0, control.error,
        discovery.state, discovery.attempts, discovery.pongsReceived > 0, discovery.dataFramesReceived > 0, discovery.error)
    fun publicEvidenceJson(): String = buildJsonObject {
        put("phase", phase); put("generation", generation)
        put("reload_requested", reloadRequested); put("reload_applied", reloadApplied)
        put("network_name_sha256", networkHash); put("device_id_sha256", deviceHash)
        put("peer_device_id_sha256", JsonArray(peerHashes.map(::JsonPrimitive)))
        put("routes", routes); put("peers", peers); put("p2p", p2p); put("failure", failure)
        fun relay(value: RelayDiagnostics) = buildJsonObject {
            put("state", value.state); put("attempts", value.attempts)
            put("registrations_sent", value.registrationsSent); put("pongs_received", value.pongsReceived)
            put("data_frames_received", value.dataFramesReceived); put("error", value.error)
            put("stage", value.stage); put("last_failure_stage", value.failedStage)
            put("dns_results", value.dnsResults); put("tcp_connections", value.tcpConnections)
            put("tls_handshakes", value.tlsHandshakes); put("websocket_upgrades", value.websocketUpgrades)
            put("tls_bytes_sent", value.tlsBytesSent); put("tls_bytes_received", value.tlsBytesReceived)
            put("errno", value.errno?.let(::JsonPrimitive) ?: JsonNull)
            put("http_status", value.httpStatus?.let(::JsonPrimitive) ?: JsonNull)
            put("dns_address_sha256", JsonArray(value.dnsHashes.map(::JsonPrimitive)))
            put("dns_benchmark_count", value.dnsBenchmarkCount)
        }
        put("relay_control", relay(control)); put("relay_discovery", relay(discovery))
    }.toString()
    companion object {
        fun parse(raw: String): DiscoveryDiagnostics {
            val obj = Json.parseToJsonElement(raw).jsonObject
            fun JsonObject.word(name: String, allowed: Set<String>, fallback: String = "") =
                (get(name) as? JsonPrimitive)?.contentOrNull?.takeIf { it in allowed } ?: fallback
            fun JsonObject.number(name: String) = (get(name) as? JsonPrimitive)?.longOrNull?.coerceAtLeast(0) ?: 0
            fun hash(value: JsonElement?) = (value as? JsonPrimitive)?.contentOrNull?.takeIf { it.matches(Regex("[a-f0-9]{64}")) } ?: ""
            val errors = setOf("profile_unavailable", "supervisor_unavailable", "component_exited", "component_panicked",
                "discovery_stopped", "startup_timeout", "p2p_unavailable", "relay_bind_unavailable")
            fun relay(name: String): RelayDiagnostics {
                val value = obj[name] as? JsonObject ?: JsonObject(emptyMap())
                return RelayDiagnostics(value.word("state", setOf("idle", "connecting", "connected", "retry_wait", "stopped"), "idle"),
                    value.number("attempts"), value.number("registrations_sent"), value.number("pongs_received"),
                    value.number("data_frames_received"), value.word("last_error", setOf("timeout", "connection", "tls", "tls_certificate", "protocol", "closed", "other")),
                    value.word("stage", setOf("idle", "dns", "tcp", "tls", "websocket", "registration", "active"), "idle"),
                    value.word("last_failure_stage", setOf("idle", "dns", "tcp", "tls", "websocket", "registration", "active")),
                    value.number("dns_results"), value.number("tcp_connections"), value.number("tls_handshakes"), value.number("websocket_upgrades"),
                    value.number("tls_bytes_sent"), value.number("tls_bytes_received"),
                    (value["last_os_error"] as? JsonPrimitive)?.intOrNull,
                    (value["http_status"] as? JsonPrimitive)?.intOrNull?.takeIf { it in 100..599 },
                    (value["dns_address_sha256"] as? JsonArray)?.take(16)?.map(::hash)?.filter(String::isNotEmpty).orEmpty(),
                    value.number("dns_benchmark_count"))
            }
            val identity = obj["identity"] as? JsonObject
            return DiscoveryDiagnostics(
                obj.word("phase", setOf("starting", "discovering", "paused", "retrying", "stopped"), "starting"),
                obj.number("runtime_generation"), obj.number("reload_requested"), obj.number("reload_applied"),
                hash(identity?.get("network_name_sha256")), hash(identity?.get("device_id_sha256")),
                (obj["peer_device_id_sha256"] as? JsonArray)?.take(64)?.map(::hash)?.filter(String::isNotEmpty).orEmpty(),
                obj.number("discovered_routes"), obj.number("peer_count"),
                obj.word("p2p_state", setOf("idle", "resolving", "running", "retry_wait", "stopped"), "idle"),
                obj.word("last_failure", errors), relay("relay_control"), relay("relay_discovery"))
        }
    }
}
