package `fun`.hackerlife.weavelo

import org.junit.Assert.*
import org.junit.Test

class WorkspaceSessionOwnershipTest {
    private val request = WorkspaceSessionRequest("192.0.2.1:39271", "device-a", "session-a")

    @Test fun recreatedUiReusesTheServiceConnectionAndSessionBinding() {
        val state = WorkspaceSessionOwnership(); val service = Any()
        assertFalse(state.attach(service, request, backendActive = false)) // open once
        repeat(4) { assertTrue(state.attach(service, request.copy(), backendActive = true)) }
        assertEquals(request, state.request())
    }

    @Test fun activityDisposalIsNotAnOwnershipReleaseAndForegroundCanReattach() {
        val state = WorkspaceSessionOwnership(); val service = Any()
        state.attach(service, request, false)
        // A UI has no Service token. Its teardown cannot stop the backend.
        assertFalse(state.release(Any(), preserveForForeground = false))
        assertEquals(request, state.request())
        assertTrue(state.attach(service, request, true))
    }

    @Test fun staleServiceTeardownCannotCloseOrForgetReplacementConnection() {
        val state = WorkspaceSessionOwnership(); val old = Any(); val replacement = Any()
        state.attach(old, request, false)
        val next = request.copy(deviceId = "device-b", sessionKey = "session-b")
        assertFalse(state.attach(replacement, next, false))
        assertFalse(state.release(old, preserveForForeground = false))
        assertEquals(next, state.request())
        assertTrue(state.release(replacement, preserveForForeground = false))
        assertNull(state.request())
    }

    @Test fun idleExpiryOnlyRetainsIntentForUserForegroundResume() {
        val state = WorkspaceSessionOwnership(); val old = Any()
        state.attach(old, request, false)
        assertTrue(state.release(old, preserveForForeground = true))
        assertEquals(request, state.request())
        assertFalse(state.release(old, preserveForForeground = false))
        assertFalse(state.attach(Any(), state.request()!!, backendActive = false))
    }

    @Test fun explicitDisconnectDoesNotResurrectOnUiRecreation() {
        val state = WorkspaceSessionOwnership(); val service = Any()
        state.attach(service, request, false)
        assertTrue(state.release(service, preserveForForeground = false))
        assertNull(state.request())
        assertFalse(state.release(service, preserveForForeground = true))
        assertNull(state.request())
    }

    @Test fun sameEndpointWithDifferentPeerOrSessionMustNeverReuseOldFileBinding() {
        for (next in listOf(request.copy(deviceId = "other-peer"), request.copy(sessionKey = "fresh-session"))) {
            val state = WorkspaceSessionOwnership(); val service = Any()
            state.attach(service, request, false)
            assertFalse(state.attach(service, next, backendActive = true))
            assertEquals(next, state.request())
        }
    }

    @Test fun failedOrSleepingBackendMustReopenEvenForTheSameUiIntent() {
        val state = WorkspaceSessionOwnership(); val service = Any()
        state.attach(service, request, false)
        assertFalse(state.attach(service, request, backendActive = false))
    }

    @Test fun explicitStopAfterIdleCannotLeaveARememberedReconnectIntent() {
        val state = WorkspaceSessionOwnership(); val service = Any()
        state.attach(service, request, false)
        state.release(service, preserveForForeground = true)
        assertTrue(state.disconnect(request.sessionKey))
        assertNull(state.request())
    }

    @Test fun staleUiDisconnectCannotStopANewPeersSession() {
        val state = WorkspaceSessionOwnership(); val service = Any()
        state.attach(service, request, false)
        val replacement = request.copy(deviceId = "device-b", sessionKey = "session-b")
        state.attach(service, replacement, false)
        assertFalse(state.disconnect(request.sessionKey))
        assertEquals(replacement, state.request())
    }
}
