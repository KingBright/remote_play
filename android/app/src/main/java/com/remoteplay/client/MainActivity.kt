package com.remoteplay.client

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.repeatOnLifecycle
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import com.remoteplay.client.ui.MobileConsoleScreen
import com.remoteplay.client.ui.MobileViewportScreen
import com.remoteplay.client.ui.QrScanScreen
import kotlinx.coroutines.delay
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

class MainActivity : ComponentActivity() {
    override fun onStart() {
        super.onStart()
        RemotePlayClient.setBackgrounded(false)
        WorkspaceClient.backgrounded(false)
    }

    override fun onStop() {
        RemotePlayClient.setBackgrounded(true)
        WorkspaceClient.backgrounded(true)
        super.onStop()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        RemotePlayClient.initialize(applicationContext)

        setContent {
            val scope = rememberCoroutineScope()
            val sessionState by RemotePlayClient.sessionState.collectAsState()
            val telemetry by RemotePlayClient.telemetry.collectAsState()
            val devices by RemotePlayClient.devices.collectAsState()
            var scanningQr by remember { mutableStateOf(false) }
            var workspaceEndpoint by remember { mutableStateOf<String?>(null) }
            var receivedFiles by remember { mutableStateOf(false) }
            var pairingMessage by remember { mutableStateOf<String?>(null) }
            val publisherStatus by PublisherClient.status.collectAsState()
            var publishAudio by remember { mutableStateOf(false) }
            val projection = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
                if (result.resultCode == RESULT_OK && result.data != null) PublisherClient.start(this, result.resultCode, result.data!!, publishAudio)
            }
            fun requestProjection(audio: Boolean) {
                publishAudio = audio
                projection.launch(getSystemService(android.media.projection.MediaProjectionManager::class.java).createScreenCaptureIntent())
            }
            val audioPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted -> requestProjection(granted) }

            LaunchedEffect(Unit) {
                lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
                    while (true) {
                        withContext(Dispatchers.IO) {
                            RemotePlayClient.refreshDevices()
                            RemotePlayClient.pollTelemetry()
                        }
                        delay(1000)
                    }
                }
            }

            when {
                workspaceEndpoint != null -> {
                    com.remoteplay.client.ui.WorkspaceScreen(workspaceEndpoint!!) { workspaceEndpoint = null }
                }
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
                        onDisconnect = {
                            scope.launch(Dispatchers.IO) { RemotePlayClient.disconnect() }
                        }
                    )
                }
                else -> {
                    MobileConsoleScreen(
                        telemetry = telemetry,
                        devices = devices,
                        nativeAvailable = RemotePlayClient.nativeAvailable,
                        pairingMessage = publisherStatus.ifBlank { pairingMessage },
                        sessionState = sessionState,
                        onCancelConnect = {
                            scope.launch(Dispatchers.IO) { RemotePlayClient.disconnect() }
                        },
                        onConnectHost = { deviceId, endpoint ->
                            scope.launch(Dispatchers.IO) { RemotePlayClient.connect(deviceId, endpoint) }
                        },
                        onConnectWorkspace = { endpoint -> workspaceEndpoint = endpoint },
                        publishing = PublisherClient.active,
                        onShareScreen = { audio ->
                            if (audio && checkSelfPermission(android.Manifest.permission.RECORD_AUDIO) != android.content.pm.PackageManager.PERMISSION_GRANTED) audioPermission.launch(android.Manifest.permission.RECORD_AUDIO)
                            else requestProjection(audio)
                        },
                        onStopSharing = { PublisherClient.stop(this) },
                        onBrowseFiles = { receivedFiles = true },
                        onJoinCode = { code -> scope.launch { pairingMessage = withContext(Dispatchers.IO) { RemotePlayClient.joinPairingPayload(code) } } },
                        onScanQr = { scanningQr = true }
                    )
                }
            }
            if (receivedFiles) com.remoteplay.client.ui.ReceivedFilesDialog { receivedFiles = false }
        }
    }
}
