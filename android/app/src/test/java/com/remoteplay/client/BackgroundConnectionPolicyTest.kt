package com.remoteplay.client

import org.junit.Assert.assertEquals
import org.junit.Test

class BackgroundConnectionPolicyTest {
    @Test fun idleBudgetHonorsExplicitSettingsAndBatteryState() {
        fun policy(limit: Int = 300, adaptive: Boolean = true, charging: Boolean = false,
                   saving: Boolean = false, battery: Int = 80, metered: Boolean = false) =
            BackgroundConnectionPolicy.seconds(limit, adaptive, charging, saving, battery, metered)
        assertEquals(300, policy())
        assertEquals(120, policy(metered = true))
        assertEquals(60, policy(battery = 10, metered = true))
        assertEquals(30, policy(saving = true, battery = 10))
        assertEquals(15, policy(limit = 15, saving = true))
        assertEquals(300, policy(charging = true, saving = true))
        assertEquals(300, policy(adaptive = false, saving = true))
        assertEquals(0, policy(limit = 0, saving = true))
    }
}
