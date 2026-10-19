package `fun`.hackerlife.weavelo.ui

import java.nio.ByteBuffer
import java.nio.file.Files
import java.nio.file.Path
import org.junit.Assert.*
import org.junit.Test

class InvitationQrTest {
    private val payload = javaClass.getResourceAsStream("/pairing-qr.txt")!!
        .bufferedReader().use { it.readLine() }

    @Test fun displayedInvitationRoundtripsThroughThePhoneScanner() {
        val matrix = invitationQr(payload)
        val side = matrix.width * 6
        val pixels = ByteArray(side * side) { i -> if (matrix[i % side / 6, i / side / 6]) 0 else 0xff.toByte() }
        assertEquals(payload, QrFrameDecoder().decode(ByteBuffer.wrap(pixels), side, side, side, 1))
    }
    @Test fun displayAndSavedImageHaveAFullWhiteQuietZone() {
        val matrix = invitationQr(payload)
        for (i in 0 until matrix.width) for (margin in 0..3) {
            assertFalse(matrix[margin, i]); assertFalse(matrix[i, margin])
            assertFalse(matrix[matrix.width - 1 - margin, i]); assertFalse(matrix[i, matrix.height - 1 - margin])
        }
    }
    @Test fun androidEncoderMatchesTheDesktopInteropFixture() {
        val matrix = invitationQr(payload)
        val fixture = buildString {
            append(payload).append('\n')
            for (y in 0 until matrix.height) {
                for (x in 0 until matrix.width) append(if (matrix[x, y]) '1' else '0')
                append('\n')
            }
        }
        if (System.getenv("WEAVELO_UPDATE_QR_FIXTURE") == "1") {
            // Explicit synthetic-only regeneration; never reads an installed profile.
            val directory = Path.of("../../remote_core/tests/fixtures").toAbsolutePath().normalize()
            assertEquals(payload, Files.readAllLines(directory.resolve("pairing-qr.txt")).first())
            Files.write(directory.resolve("android-pairing-qr.txt"), fixture.toByteArray(Charsets.UTF_8))
        } else {
            val expected = javaClass.getResourceAsStream("/android-pairing-qr.txt")!!.bufferedReader()
                .use { it.readLines().joinToString("\n", postfix = "\n") }
            assertEquals(expected, fixture)
        }
    }
}
