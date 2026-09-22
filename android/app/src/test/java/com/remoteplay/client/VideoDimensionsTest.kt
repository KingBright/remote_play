package com.remoteplay.client

import org.junit.Assert.*
import org.junit.Test

class VideoDimensionsTest {
    @Test fun encoderSizeFitsBoundsAndHardwareAlignment() {
        assertEquals(VideoDimensions(320, 720), VideoDimensions.fitEncoder(1080, 2400, 1280, 720, 16, 16))
        assertEquals(VideoDimensions(848, 464), VideoDimensions.fitEncoder(853, 479, 1280, 720, 16, 16))
        assertEquals(VideoDimensions(853, 479), VideoDimensions.fitEncoder(853, 479, 1280, 720, 1, 1))
        assertNull(VideoDimensions.fitEncoder(100, 200, 1, 1, 16, 16))
    }
    @Test fun supportsPortraitSquareAndUltrawideFramesWithoutForcingSixteenByNine() {
        for ((width, height) in listOf(1080 to 1920, 913 to 913, 3440 to 1440, 853 to 479)) {
            val dimensions = VideoDimensions.fromDecodedFrame(width, height)!!
            assertEquals(width, dimensions.width)
            assertEquals(height, dimensions.height)
            assertEquals(width.toFloat() / height, dimensions.aspectRatio, 0.00001f)
        }
    }

    @Test fun codecPaddingIsExcludedUsingInclusiveCropBounds() {
        val dimensions = VideoDimensions.fromDecodedFrame(1920, 1088, 0, 0, 1919, 1079)!!
        assertEquals(VideoDimensions(1920, 1080), dimensions)
        assertEquals(VideoDimensions(853, 479),
            VideoDimensions.fromDecodedFrame(864, 480, 8, 1, 860, 479))
    }

    @Test fun invalidCropFallsBackToCodedSizeAndInvalidSizeIsIgnored() {
        assertEquals(VideoDimensions(640, 480), VideoDimensions.fromDecodedFrame(640, 480, -1, 0, 639, 479))
        assertEquals(VideoDimensions(640, 480), VideoDimensions.fromDecodedFrame(640, 480, 20, 0, 10, 479))
        assertNull(VideoDimensions.fromDecodedFrame(0, 480))
        assertNull(VideoDimensions.fromDecodedFrame(640, -1))
    }
}
