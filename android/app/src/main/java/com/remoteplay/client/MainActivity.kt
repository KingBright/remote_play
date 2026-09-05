package com.remoteplay.client

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import com.remoteplay.client.ui.MobileConsoleScreen
import com.remoteplay.client.ui.MobileViewportScreen
import com.remoteplay.client.ui.QrScanScreen
import kotlinx.coroutines.delay

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        setContent {
            val sessionState by RemotePlayClient.sessionState.collectAsState()
            val telemetry by RemotePlayClient.telemetry.collectAsState()
            val devices by RemotePlayClient.devices.collectAsState()
            var scanningQr by remember { mutableStateOf(false) }
            var pairingMessage by remember { mutableStateOf<String?>(null) }

            LaunchedEffect(Unit) {
                while (true) {
                    RemotePlayClient.refreshDevices()
                    RemotePlayClient.pollTelemetry()
                    delay(1000)
                }
            }

            when {
                scanningQr -> {
                    QrScanScreen(
                        onResult = { raw ->
                            scanningQr = false
                            pairingMessage = RemotePlayClient.joinPairingPayload(raw)
                        },
                        onCancel = { scanningQr = false }
                    )
                }
                sessionState == SessionState.STREAMING || sessionState == SessionState.RECONNECTING -> {
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
                        devices = devices,
                        nativeAvailable = RemotePlayClient.nativeAvailable,
                        pairingMessage = pairingMessage,
                        onConnectHost = { deviceId, endpoint ->
                            RemotePlayClient.connect(deviceId, endpoint)
                        },
                        onScanQr = { scanningQr = true }
                    )
                }
            }
        }
    }
}
