package `fun`.hackerlife.weavelo

import kotlinx.serialization.json.*

/** Public runtime observations only. Never parse invitations, capabilities or endpoint URLs. */
data class RelayDiagnostics(
    val state: String = "idle", val attempts: Long = 0, val registrationsSent: Long = 0,
    val pongsReceived: Long = 0, val dataFramesReceived: Long = 0, val error: String = ""
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
                    value.number("data_frames_received"), value.word("last_error", setOf("timeout", "connection", "tls", "protocol", "closed", "other")))
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
