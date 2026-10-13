package com.remoteplay.client

/** Network membership is independent of whether the device currently publishes media. */
data class HostDevice(
    val id: String,
    val name: String,
    val endpoint: String,
    val scope: String,
    val canStream: Boolean,
    val online: Boolean,
    val canView: Boolean = false,
    val fileTransfer: Boolean = false,
    val clipboardSync: Boolean = false,
    val hasService: Boolean = false
) {
    val canOpenWorkspace: Boolean get() = online && hasService && (canStream || fileTransfer)
    val canViewScreen: Boolean get() = online && hasService && canStream
    val canSendFiles: Boolean get() = online && hasService && fileTransfer
    val capabilitySummary: String get() = buildList {
        if (canStream) add("Screen sharing")
        if (fileTransfer) add("Files")
        if (canView) add("Can view other devices")
        if (!hasService) add("No incoming service active")
    }.joinToString(" · ").ifBlank { "No supported action advertised" }
}

/** A named device must resolve through discovery; never reuse a stale relay/P2P port. */
fun resolveNetworkEndpoint(deviceId: String, manualEndpoint: String, devices: List<HostDevice>): String {
    if (deviceId.isBlank()) return manualEndpoint
    val device = devices.firstOrNull { it.id == deviceId }
    check(device != null && device.canOpenWorkspace) {
        "This device is not currently reachable. Wait for discovery and reconnect."
    }
    return device.endpoint
}
