package com.remoteplay.client

// JSONObject.NULL and absent properties are deliberately not zero. Reject
// non-finite/negative diagnostics rather than presenting invented precision.
internal fun nullableTelemetryNumber(value: Any?): Double? =
    (value as? Number)?.toDouble()?.takeIf { it.isFinite() && it >= 0.0 }
