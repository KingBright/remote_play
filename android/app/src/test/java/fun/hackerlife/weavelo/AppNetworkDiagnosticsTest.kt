package `fun`.hackerlife.weavelo

import org.junit.Assert.*
import org.junit.Test

class AppNetworkDiagnosticsTest {
    private val key = "dGhlIHNhbXBsZSBub25jZQ=="
    private val valid = "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n"
    @Test fun upgradeMustMatchTheStandardHandshake() {
        assertEquals(101 to true, PlatformRelayProbe.validateUpgrade(valid, key))
        assertFalse(PlatformRelayProbe.validateUpgrade(valid.replace("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=", "wrong"), key).second)
        assertFalse(PlatformRelayProbe.validateUpgrade(valid.replace("Upgrade: websocket\r\n", ""), key).second)
    }
    @Test fun rejectedAndOversizedResponsesNeverBecomeSuccess() {
        assertEquals(403 to false, PlatformRelayProbe.validateUpgrade("HTTP/1.1 403 Forbidden\r\nX-Secret: canary\r\n\r\n", key))
        assertEquals(0 to false, PlatformRelayProbe.validateUpgrade("x".repeat(8193), key))
    }
    @Test fun applicationAndProductionEvidenceRemainSeparateAndWhitelisted() {
        val native = DiscoveryDiagnostics.parse("""{"phase":"discovering","relay_control":{"stage":"tls","last_failure_stage":"tls","dns_results":1,"tcp_connections":2,"last_os_error":104,"http_status":999,"tls_bytes_sent":420,"tls_bytes_received":0,"raw_error":"secret-canary"}}""")
        val network = AppNetworkStatus(uid = 12345, activeNetwork = 9, callbackNetwork = 9, blocked = false, transports = listOf("wifi"))
        val evidence = publicConnectionEvidence(network, native)
        assertTrue(evidence.contains("application_uid"))
        assertTrue(evidence.contains("last_failure_stage"))
        assertEquals("tls", native.control.failedStage)
        assertEquals(104, native.control.errno)
        assertNull(native.control.httpStatus)
        assertFalse(evidence.contains("secret-canary"))
        assertFalse(network.observationError)
        assertNull(AppNetworkStatus().blocked)
    }
}
