package `fun`.hackerlife.weavelo.ui

import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.MultiFormatReader
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.ReaderException
import com.google.zxing.common.HybridBinarizer
import java.nio.ByteBuffer
import java.util.concurrent.atomic.AtomicBoolean

/** One decoder per serial camera executor; decoded text is never logged here. */
internal class QrFrameDecoder {
    private val reader = MultiFormatReader().apply {
        setHints(mapOf(DecodeHintType.POSSIBLE_FORMATS to listOf(BarcodeFormat.QR_CODE)))
    }

    fun decode(
        buffer: ByteBuffer, width: Int, height: Int, rowStride: Int, pixelStride: Int,
        left: Int = 0, top: Int = 0, cropWidth: Int = width, cropHeight: Int = height
    ): String? {
        val bytes = packLuminance(buffer, width, height, rowStride, pixelStride,
            left, top, cropWidth, cropHeight) ?: return null
        val source = PlanarYUVLuminanceSource(bytes, cropWidth, cropHeight,
            0, 0, cropWidth, cropHeight, false)
        return try {
            reader.decodeWithState(BinaryBitmap(HybridBinarizer(source))).text
        } catch (_: ReaderException) {
            null
        } finally {
            reader.reset()
        }
    }
}

/** CameraX planes may have row padding, pixel strides and a nonzero crop/position. */
internal fun packLuminance(
    buffer: ByteBuffer, width: Int, height: Int, rowStride: Int, pixelStride: Int,
    left: Int = 0, top: Int = 0, cropWidth: Int = width, cropHeight: Int = height
): ByteArray? {
    if (width <= 0 || height <= 0 || rowStride <= 0 || pixelStride <= 0 ||
        left < 0 || top < 0 || cropWidth <= 0 || cropHeight <= 0 ||
        cropWidth > width - left || cropHeight > height - top ||
        cropWidth.toLong() * cropHeight > 16_777_216L ||
        (width - 1L) * pixelStride + 1 > rowStride) return null
    val start = buffer.position().toLong() + top.toLong() * rowStride + left.toLong() * pixelStride
    val last = start + (cropHeight - 1L) * rowStride + (cropWidth - 1L) * pixelStride
    if (last >= buffer.limit()) return null
    val input = buffer.duplicate()
    val packed = ByteArray(cropWidth * cropHeight)
    for (y in 0 until cropHeight) {
        val row = (start + y.toLong() * rowStride).toInt()
        if (pixelStride == 1) {
            input.position(row)
            input.get(packed, y * cropWidth, cropWidth)
        } else {
            for (x in 0 until cropWidth) packed[y * cropWidth + x] = input.get(row + x * pixelStride)
        }
    }
    return packed
}

/** Fences duplicate frames and callbacks arriving after back/cancel/disposal. */
internal class QrScanSession {
    private val active = AtomicBoolean(true)
    private val claimed = AtomicBoolean(false)
    val isActive: Boolean get() = active.get()
    val acceptsFrames: Boolean get() = isActive && !claimed.get()
    fun claimResult(): Boolean = isActive && claimed.compareAndSet(false, true)
    fun close() { active.set(false) }
}
