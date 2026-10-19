package `fun`.hackerlife.weavelo.ui

import com.google.zxing.BarcodeFormat
import com.google.zxing.EncodeHintType
import com.google.zxing.common.BitMatrix
import com.google.zxing.qrcode.QRCodeWriter
import com.google.zxing.qrcode.decoder.ErrorCorrectionLevel

/** Renders the native codec's payload; contains no alternate invitation parser/authentication. */
internal fun invitationQr(payload: String): BitMatrix {
    require(payload.isNotBlank() && payload.length <= 4096) { "Invitation unavailable" }
    return QRCodeWriter().encode(payload, BarcodeFormat.QR_CODE, 0, 0,
        mapOf(EncodeHintType.MARGIN to 4, EncodeHintType.ERROR_CORRECTION to ErrorCorrectionLevel.M))
}
