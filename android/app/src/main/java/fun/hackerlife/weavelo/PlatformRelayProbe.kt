package `fun`.hackerlife.weavelo

import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.Socket
import java.net.SocketTimeoutException
import java.security.MessageDigest
import java.security.SecureRandom
import java.util.Base64
import java.util.concurrent.TimeUnit
import java.util.concurrent.Executors
import java.util.concurrent.ExecutionException
import java.util.concurrent.TimeoutException
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import javax.net.ssl.SNIHostName
import javax.net.ssl.SSLHandshakeException
import javax.net.ssl.SSLPeerUnverifiedException
import javax.net.ssl.SSLProtocolException
import javax.net.ssl.SSLSocket
import javax.net.ssl.SSLSocketFactory
import kotlinx.serialization.json.*

data class PlatformRelayProbeResult(val phase: String = "idle", val error: String = "",
    val dnsCount: Int = 0, val tcpConnected: Boolean = false, val tlsVerified: Boolean = false,
    val httpStatus: Int = 0, val websocketVerified: Boolean = false,
    val dnsHashes: List<String> = emptyList(), val dnsBenchmarkCount: Int = 0,
    val errorType: String = "", val causeType: String = "", val osError: Int? = null) {
    fun publicEvidenceJson(): String = buildJsonObject {
        put("phase", phase); put("error", error); put("dns_count", dnsCount)
        put("tcp_connected", tcpConnected); put("tls_verified", tlsVerified)
        put("http_status", httpStatus); put("websocket_verified", websocketVerified)
        put("dns_address_sha256", JsonArray(dnsHashes.map(::JsonPrimitive))); put("dns_benchmark_count", dnsBenchmarkCount)
        put("dns_source", "java_getaddrinfo")
        put("error_type", errorType); put("cause_type", causeType)
        put("os_error", osError?.let(::JsonPrimitive) ?: JsonNull)
    }.toString()
}

/** Independent Android-UID control. Sends no group registration, cookie or credential. */
enum class ProbeDestination(val host: String, val port: Int, val websocket: Boolean) {
    RELAY("relay.hackerlife.fun", 8443, true), HTTPS_CONTROL("example.com", 443, false)
}

class PlatformRelayProbe(private val destination: ProbeDestination = ProbeDestination.RELAY) {
    val result = AtomicReference(PlatformRelayProbeResult())
    private val running = AtomicBoolean(false)
    private val dnsRunning = AtomicBoolean(false)
    private val dnsExecutor = Executors.newSingleThreadExecutor { action ->
        Thread(action, "WeaveloProbeDns").apply { isDaemon = true }
    }
    fun start() {
        if (!running.compareAndSet(false, true)) return
        Thread({
            var status = PlatformRelayProbeResult(phase = "dns")
            fun publish() { result.set(status) }
            var socket: Socket? = null
            try {
                publish()
                val addresses = resolveDefaultUidNetwork()
                status = status.copy(phase = "tcp", dnsCount = addresses.size,
                    dnsHashes = addresses.take(16).map { addr -> MessageDigest.getInstance("SHA-256").digest(addr.address).joinToString("") { "%02x".format(it.toInt() and 255) } },
                    dnsBenchmarkCount = addresses.count { addr -> val raw = addr.address; raw.size == 4 && (raw[0].toInt() and 255) == 198 && (raw[1].toInt() and 255) in 18..19 }); publish()
                var last: Exception? = null
                val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(4)
                for (address in addresses) {
                    val attempt = Socket()
                    val remaining = TimeUnit.NANOSECONDS.toMillis(deadline - System.nanoTime()).coerceAtLeast(1).toInt()
                    try { attempt.connect(InetSocketAddress(address, destination.port), remaining); socket = attempt; break }
                    catch (error: Exception) { last = error; attempt.close() }
                    if (System.nanoTime() >= deadline) break
                }
                val connected = socket ?: throw (last ?: java.net.UnknownHostException())
                status = status.copy(phase = "tls", tcpConnected = true); publish()
                val tls = (SSLSocketFactory.getDefault() as SSLSocketFactory)
                    .createSocket(connected, destination.host, destination.port, true) as SSLSocket
                socket = tls
                tls.soTimeout = 4000
                tls.sslParameters = tls.sslParameters.apply {
                    endpointIdentificationAlgorithm = "HTTPS"
                    serverNames = listOf(SNIHostName(destination.host))
                }
                tls.startHandshake()
                status = status.copy(phase = if (destination.websocket) "websocket" else "https", tlsVerified = true); publish()
                val nonce = ByteArray(16).also { SecureRandom().nextBytes(it) }
                val key = Base64.getEncoder().encodeToString(nonce)
                val request = if (destination.websocket) "GET /v1/relay HTTP/1.1\r\nHost: ${destination.host}:${destination.port}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: $key\r\n\r\n"
                    else "GET / HTTP/1.1\r\nHost: ${destination.host}\r\nConnection: close\r\n\r\n"
                tls.outputStream.write(request.toByteArray(Charsets.US_ASCII)); tls.outputStream.flush()
                val header = java.io.ByteArrayOutputStream()
                val end = System.nanoTime() + TimeUnit.SECONDS.toNanos(4)
                while (header.size() < 8192) {
                    if (System.nanoTime() >= end) throw SocketTimeoutException()
                    val value = tls.inputStream.read()
                    if (value < 0) throw java.io.EOFException()
                    header.write(value)
                    val bytes = header.toByteArray()
                    if (bytes.size >= 4 && bytes.takeLast(4) == listOf<Byte>(13, 10, 13, 10)) break
                }
                val response = validateUpgrade(header.toString("US-ASCII"), key)
                status = status.copy(phase = "complete", httpStatus = response.first, websocketVerified = response.second,
                    error = if (response.second || (!destination.websocket && response.first in 200..399)) "" else "response_rejected")
            } catch (error: Exception) {
                val causes = generateSequence<Throwable>(error) { it.cause }.take(6).toList()
                val safeType: (Throwable?) -> String = { value -> value?.javaClass?.simpleName?.takeIf { it.matches(Regex("[A-Za-z0-9_]{1,64}")) }.orEmpty() }
                status = status.copy(errorType = safeType(error), causeType = safeType(error.cause),
                    osError = causes.filterIsInstance<android.system.ErrnoException>().firstOrNull()?.errno,
                    error = when (error) {
                    is java.net.UnknownHostException -> "dns_failed"
                    is SocketTimeoutException -> "timeout"
                    is SSLPeerUnverifiedException -> "certificate"
                    is SSLHandshakeException -> when {
                        causes.any { it is java.io.EOFException } -> "closed_during_tls"
                        causes.any { it is java.security.cert.CertPathValidatorException } -> "certificate"
                        else -> "tls_handshake"
                    }
                    is SSLProtocolException -> "tls_protocol"
                    is java.io.EOFException -> "closed"
                    is java.net.ConnectException -> "connect_failed"
                    is SecurityException -> "permission"
                    else -> "io_error"
                })
            } finally {
                runCatching { socket?.close() }; publish(); running.set(false)
            }
        }, "WeaveloRelayProbe").apply { isDaemon = true; start() }
    }
    private fun resolveDefaultUidNetwork(): List<InetAddress> {
        // One outstanding resolver per probe, including when libc ignores interruption.
        if (!dnsRunning.compareAndSet(false, true)) throw java.io.IOException()
        val future = dnsExecutor.submit<List<InetAddress>> {
            try { InetAddress.getAllByName(destination.host).toList() }
            finally { dnsRunning.set(false) }
        }
        return try { future.get(4, TimeUnit.SECONDS).ifEmpty { throw java.net.UnknownHostException() } }
        catch (_: TimeoutException) {
            // libc may ignore interruption; retain the single in-flight task until it finishes.
            throw SocketTimeoutException()
        }
        catch (error: ExecutionException) { throw (error.cause as? Exception ?: java.io.IOException()) }
    }
    companion object {
        fun validateUpgrade(header: String, key: String): Pair<Int, Boolean> {
            if (header.length > 8192 || !header.endsWith("\r\n\r\n")) return 0 to false
            val lines = header.split("\r\n")
            val status = Regex("HTTP/1[.][01] ([0-9]{3})(?: .*)?").matchEntire(lines.firstOrNull().orEmpty())?.groupValues?.get(1)?.toIntOrNull() ?: 0
            val expected = Base64.getEncoder().encodeToString(MessageDigest.getInstance("SHA-1")
                .digest((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").toByteArray(Charsets.US_ASCII)))
            val accept = lines.drop(1).firstOrNull { it.substringBefore(':').equals("Sec-WebSocket-Accept", true) }?.substringAfter(':')?.trim()
            val upgrade = lines.drop(1).firstOrNull { it.substringBefore(':').equals("Upgrade", true) }?.substringAfter(':')?.trim()
            val connection = lines.drop(1).firstOrNull { it.substringBefore(':').equals("Connection", true) }?.substringAfter(':')?.split(',')?.any { it.trim().equals("Upgrade", true) } == true
            return status to (status == 101 && accept == expected && upgrade.equals("websocket", true) && connection)
        }
    }
}
