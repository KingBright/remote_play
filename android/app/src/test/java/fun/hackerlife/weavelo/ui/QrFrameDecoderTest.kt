package `fun`.hackerlife.weavelo.ui

import com.google.zxing.BarcodeFormat
import com.google.zxing.MultiFormatWriter
import java.nio.ByteBuffer
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.*
import org.junit.Test

class QrFrameDecoderTest {
    private val fixture = javaClass.getResourceAsStream("/pairing-qr.txt")!!
        .bufferedReader().use { it.readLines() }
    private val payload = fixture.first()
    private val modules = fixture.drop(1)

    private fun raster(turns: Int = 0, scale: Int = 3): Pair<ByteArray, Int> {
        val quiet = 4
        val side = (modules.size + 2 * quiet) * scale
        val data = ByteArray(side * side) { 0xff.toByte() }
        for (y in 0 until side) for (x in 0 until side) {
            var sx = x
            var sy = y
            repeat(turns) { val oldX = sx; sx = sy; sy = side - 1 - oldX }
            val mx = sx / scale - quiet
            val my = sy / scale - quiet
            if (my in modules.indices && mx in modules.indices && modules[my][mx] == '1') data[y * side + x] = 0
        }
        return data to side
    }

    @Test fun rustGeneratedInvitationDecodesInEveryPhoneOrientation() {
        for (scale in listOf(3, 6)) for (turns in 0..3) {
            val (pixels, side) = raster(turns, scale)
            assertEquals(payload, QrFrameDecoder().decode(ByteBuffer.wrap(pixels), side, side, side, 1))
        }
    }

    @Test fun rowPaddingPixelStrideCropAndBufferPositionPreserveThePayload() {
        val (pixels, side) = raster()
        for (stride in listOf(1, 2)) {
            val width = side + 12
            val height = side + 8
            val rowStride = width * stride + 31
            val prefix = 7
            // Last-row padding need not be present in a CameraX plane.
            val input = ByteArray(prefix + (height - 1) * rowStride + (width - 1) * stride + 1) { 0xff.toByte() }
            for (y in 0 until side) for (x in 0 until side) {
                input[prefix + (y + 3) * rowStride + (x + 5) * stride] = pixels[y * side + x]
            }
            val buffer = ByteBuffer.wrap(input).apply { position(prefix) }
            assertEquals(payload, QrFrameDecoder().decode(buffer, width, height, rowStride, stride, 5, 3, side, side))
            assertEquals(prefix, buffer.position())
            assertEquals(input.size, buffer.limit())
        }
    }

    @Test fun unreadableFramesAndOtherBarcodeTypesDoNotCompleteTheScan() {
        assertNull(QrFrameDecoder().decode(ByteBuffer.wrap(ByteArray(200 * 200)), 200, 200, 200, 1))
        assertNull(packLuminance(ByteBuffer.allocate(3), 2, 2, 2, 1))
        assertNull(packLuminance(ByteBuffer.allocate(8), 2, 2, 1, 1))
        assertNull(packLuminance(ByteBuffer.allocate(8), 2, 2, 2, 1, 2, 0, 1, 1))
        val barcode = MultiFormatWriter().encode("1234567890", BarcodeFormat.CODE_128, 300, 120)
        val bytes = ByteArray(barcode.width * barcode.height) { i -> if (barcode[i % barcode.width, i / barcode.width]) 0 else 0xff.toByte() }
        assertNull(QrFrameDecoder().decode(ByteBuffer.wrap(bytes), barcode.width, barcode.height, barcode.width, 1))
    }

    @Test fun unrelatedQrTextIsLeftForTheSharedInvitationValidator() {
        // The Rust malformed-wrapper regression rejects this exact text. The
        // camera must not invent a second credential parser or silently alter it.
        val unrelated = "https://example.invalid/?code=one"
        val qr = MultiFormatWriter().encode(unrelated, BarcodeFormat.QR_CODE, 240, 240)
        val bytes = ByteArray(qr.width * qr.height) { i -> if (qr[i % qr.width, i / qr.width]) 0 else 0xff.toByte() }
        assertEquals(unrelated, QrFrameDecoder().decode(ByteBuffer.wrap(bytes), qr.width, qr.height, qr.width, 1))
    }

    @Test fun onlyOneConcurrentFrameCanClaimTheJoin() {
        val session = QrScanSession()
        val executor = Executors.newFixedThreadPool(4)
        val ready = CountDownLatch(1)
        val done = CountDownLatch(32)
        val claims = AtomicInteger()
        try {
            repeat(32) { executor.execute { ready.await(); if (session.claimResult()) claims.incrementAndGet(); done.countDown() } }
            ready.countDown()
            assertTrue(done.await(5, TimeUnit.SECONDS))
            assertEquals(1, claims.get())
            assertFalse(session.acceptsFrames)
        } finally { executor.shutdownNow() }
    }

    @Test fun cancellingOrLeavingTheScannerRejectsPendingAndFutureResults() {
        val cancelled = QrScanSession()
        cancelled.close()
        assertFalse(cancelled.claimResult())
        val pending = QrScanSession()
        assertTrue(pending.claimResult())
        pending.close()
        assertFalse(pending.isActive) // Main-thread delivery checks this after executor handoff.
        assertFalse(pending.acceptsFrames)
        assertTrue(QrScanSession().claimResult()) // A fresh scan can retry an invalid invitation.
    }
}
