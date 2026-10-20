package `fun`.hackerlife.weavelo

import org.junit.Assert.*
import org.junit.Test

class DiscoveryDiagnosticsTest {
    @Test fun unknownAndPrivateFieldsNeverReachEvidence() {
        val raw = """{"phase":"secret-canary","identity":{"network_name_sha256":"secret-canary"},"invitation":"secret-canary","relay_control":{"last_error":"secret-canary","registrations_sent":-9}}"""
        val value = DiscoveryDiagnostics.parse(raw)
        assertEquals("starting", value.phase)
        assertEquals(0L, value.control.registrationsSent)
        assertFalse(value.publicEvidenceJson().contains("secret-canary"))
    }
    @Test fun localRegistrationIsNotServerResponseOrDiscovery() {
        val value = DiscoveryDiagnostics.parse("""{"phase":"discovering","relay_control":{"state":"connected","registrations_sent":3}}""")
        assertEquals(3L, value.control.registrationsSent)
        assertEquals(0L, value.control.pongsReceived)
        assertEquals(0L, value.peers)
        assertNotEquals(value.transitionKey(), value.copy(control = value.control.copy(pongsReceived = 1)).transitionKey())
    }
    @Test fun pendingConfigurationAndRetriesRetainPairing() {
        assertTrue(DiscoveryDiagnostics(reloadRequested = 2, reloadApplied = 1).emptyMessage.contains("pairing is retained"))
        assertTrue(DiscoveryDiagnostics(phase = "retrying").emptyMessage.contains("existing pairing"))
        assertFalse(DiscoveryDiagnostics().emptyMessage.contains("online"))
    }
}
