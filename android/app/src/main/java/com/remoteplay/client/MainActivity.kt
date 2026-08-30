package com.remoteplay.client

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import com.remoteplay.client.ui.MobileConsoleScreen
import com.remoteplay.client.ui.MobileViewportScreen

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        setContent {
            val sessionState by RemotePlayClient.sessionState.collectAsState()
            val telemetry by RemotePlayClient.telemetry.collectAsState()

            when (sessionState) {
                SessionState.STREAMING -> {
                    MobileViewportScreen(
                        telemetry = telemetry,
                        onDisconnect = {
                            RemotePlayClient.disconnect()
                        }
                    )
                }
                else -> {
                    MobileConsoleScreen(
                        telemetry = telemetry,
                        onConnectHost = { deviceId ->
                            RemotePlayClient.connect(deviceId, "10.144.0.5:8000")
                        },
                        onScanQr = {
                            // 扫码免密加入 Mesh 组网
                        }
                    )
                }
            }
        }
    }
}
