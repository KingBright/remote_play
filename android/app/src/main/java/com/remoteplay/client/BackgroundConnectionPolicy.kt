package com.remoteplay.client

import android.content.Context
import android.net.ConnectivityManager
import android.os.BatteryManager
import android.os.PowerManager

/** Idle media connections only; unfinished file transfers defer this deadline. */
object BackgroundConnectionPolicy {
    fun seconds(configured: Int, adaptive: Boolean, charging: Boolean, powerSave: Boolean,
                batteryPercent: Int, metered: Boolean): Int {
        val limit = configured.coerceIn(0, 86400)
        if (limit == 0 || !adaptive || charging) return limit // Explicit "always keep" wins.
        return minOf(limit, when {
            powerSave -> 30
            batteryPercent in 0..15 -> 60
            metered -> 120
            else -> limit
        })
    }

    fun currentSeconds(context: Context): Int {
        val prefs = context.getSharedPreferences("stream", Context.MODE_PRIVATE)
        val battery = context.getSystemService(BatteryManager::class.java)
        return seconds(prefs.getInt("background_keepalive_seconds", 300),
            prefs.getBoolean("adaptive_background", true), battery.isCharging,
            context.getSystemService(PowerManager::class.java).isPowerSaveMode,
            battery.getIntProperty(BatteryManager.BATTERY_PROPERTY_CAPACITY),
            context.getSystemService(ConnectivityManager::class.java).isActiveNetworkMetered)
    }
}
