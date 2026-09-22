package com.remoteplay.client

/** Visible pixels can differ from the codec's padded buffer dimensions. */
data class VideoDimensions(val width: Int, val height: Int) {
    init { require(width > 0 && height > 0) }
    val aspectRatio: Float get() = width.toFloat() / height

    companion object {
        fun fitEncoder(sourceWidth: Int, sourceHeight: Int, boundWidth: Int, boundHeight: Int,
                       widthAlignment: Int, heightAlignment: Int): VideoDimensions? {
            if (minOf(sourceWidth, sourceHeight, boundWidth, boundHeight, widthAlignment, heightAlignment) <= 0) return null
            val scale = minOf(1.0, boundWidth.toDouble() / sourceWidth, boundHeight.toDouble() / sourceHeight)
            val width = (sourceWidth * scale).toInt() / widthAlignment * widthAlignment
            val height = (sourceHeight * scale).toInt() / heightAlignment * heightAlignment
            return if (width > 0 && height > 0) VideoDimensions(width, height) else null
        }

        fun fromDecodedFrame(
            width: Int, height: Int,
            cropLeft: Int = 0, cropTop: Int = 0,
            cropRight: Int = width - 1, cropBottom: Int = height - 1
        ): VideoDimensions? {
            if (width <= 0 || height <= 0) return null
            // MediaCodec crop right/bottom coordinates are inclusive.
            return if (cropLeft in 0 until width && cropRight in cropLeft until width &&
                cropTop in 0 until height && cropBottom in cropTop until height) {
                VideoDimensions(cropRight - cropLeft + 1, cropBottom - cropTop + 1)
            } else VideoDimensions(width, height)
        }
    }
}
