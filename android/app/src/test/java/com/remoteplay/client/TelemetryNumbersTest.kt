package com.remoteplay.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class TelemetryNumbersTest {
    @Test fun unavailableAndInvalidMeasurementsStayNull() {
        for (value in listOf(null, Any(), "0", -1, Double.NaN, Double.POSITIVE_INFINITY)) {
            assertNull(nullableTelemetryNumber(value))
        }
    }
    @Test fun measuredZeroAndFiniteMillisecondsArePreserved() {
        assertEquals(0.0, nullableTelemetryNumber(0))
        assertEquals(12.5, nullableTelemetryNumber(12.5))
    }
}
