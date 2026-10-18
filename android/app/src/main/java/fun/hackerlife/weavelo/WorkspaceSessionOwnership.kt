package `fun`.hackerlife.weavelo

/** Process-local intent, not credentials or a promise to survive force-stop. */
data class WorkspaceSessionRequest(val endpoint: String, val deviceId: String, val sessionKey: String)

/** A stale Service teardown must never close a replacement owner's connection. */
class WorkspaceSessionOwnership {
    private var owner: Any? = null
    private var desired: WorkspaceSessionRequest? = null

    @Synchronized fun request(): WorkspaceSessionRequest? = desired

    @Synchronized fun attach(token: Any, request: WorkspaceSessionRequest, backendActive: Boolean): Boolean {
        val reuse = backendActive && desired?.endpoint == request.endpoint &&
            desired?.deviceId == request.deviceId && desired?.sessionKey == request.sessionKey
        owner = token
        desired = request
        return reuse
    }

    @Synchronized fun release(token: Any, preserveForForeground: Boolean): Boolean {
        if (owner !== token) return false
        owner = null
        if (!preserveForForeground) desired = null
        return true
    }

    @Synchronized fun disconnect(sessionKey: String): Boolean {
        if (desired?.sessionKey != sessionKey) return false
        owner = null
        desired = null
        return true
    }
}
