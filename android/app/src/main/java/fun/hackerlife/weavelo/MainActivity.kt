package `fun`.hackerlife.weavelo

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
import `fun`.hackerlife.weavelo.ui.MobileConsoleScreen
import `fun`.hackerlife.weavelo.ui.MobileViewportScreen
import `fun`.hackerlife.weavelo.ui.QrScanScreen
import kotlinx.coroutines.delay
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

class MainActivity : ComponentActivity() {
    override fun onStart() {
        super.onStart()
        WeaveloClient.setBackgrounded(false)
        WorkspaceClient.backgrounded(false)
    }

    override fun onStop() {
        WeaveloClient.setBackgrounded(true)
        WorkspaceClient.backgrounded(true)
        super.onStop()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        WeaveloClient.initialize(applicationContext)

        setContent {
            val scope = rememberCoroutineScope()
            val sessionState by WeaveloClient.sessionState.collectAsState()
            val telemetry by WeaveloClient.telemetry.collectAsState()
            val sessionError by WeaveloClient.sessionError.collectAsState()
            val devices by WeaveloClient.devices.collectAsState()
            val networkName by WeaveloClient.networkName.collectAsState()
            var joiningNetwork by remember { mutableStateOf(false) }
            var sendFileOnConnect by remember { mutableStateOf(false) }
            var workspaceDeviceId by remember { mutableStateOf("") }
            var scanningQr by remember { mutableStateOf(false) }
            var workspaceEndpoint by remember { mutableStateOf<String?>(null) }
            var receivedFiles by remember { mutableStateOf(false) }
            var pairingMessage by remember { mutableStateOf<String?>(null) }
            val publisherStatus by PublisherClient.status.collectAsState()
            val receiverState by FileReceiverClient.state.collectAsState()
            fun joinNetwork(raw: String) {
                if (joiningNetwork) return
                if (WorkspaceClient.active || PublisherClient.active || FileReceiverClient.ownsNetwork ||
                    sessionState in listOf(SessionState.CONNECTING, SessionState.STREAMING, SessionState.RECONNECTING)) {
                    pairingMessage = "error:Disconnect active sessions and stop file reception and screen sharing before changing networks."
                    return
                }
                joiningNetwork = true
                pairingMessage = "Joining device network..."
                scope.launch {
                    try {
                        pairingMessage = withContext(Dispatchers.IO) { WeaveloClient.joinPairingPayload(raw) }
                    } catch (cancelled: kotlinx.coroutines.CancellationException) {
                        throw cancelled
                    } catch (error: Exception) {
                        pairingMessage = "error:${error.message ?: "Could not join network"}"
                    } finally {
                        joiningNetwork = false
                    }
                }
            }
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
                            WeaveloClient.refreshDevices()
                            WeaveloClient.pollTelemetry()
                        }
                        delay(1000)
                    }
                }
            }

            when {
                workspaceEndpoint != null -> {
                    `fun`.hackerlife.weavelo.ui.WorkspaceScreen(workspaceEndpoint!!, sendFileOnConnect, workspaceDeviceId) { workspaceEndpoint = null }
                }
                scanningQr -> {
                    QrScanScreen(
                        onResult = { raw ->
                            scanningQr = false
                            joinNetwork(raw)
                        },
                        onCancel = { scanningQr = false }
                    )
                }
                sessionState == SessionState.STREAMING || sessionState == SessionState.RECONNECTING -> {
                    MobileViewportScreen(
                        onDisconnect = {
                            scope.launch(Dispatchers.IO) { WeaveloClient.disconnect() }
                        }
                    )
                }
                else -> {
                    MobileConsoleScreen(
                        telemetry = telemetry,
                        devices = devices,
                        nativeAvailable = WeaveloClient.nativeAvailable && !joiningNetwork,
                        networkName = networkName,
                        joiningNetwork = joiningNetwork,
                        pairingMessage = listOfNotNull(pairingMessage, publisherStatus.takeIf { it.isNotBlank() }, receiverState.message.takeIf { it.isNotBlank() }).distinct().joinToString("\n"),
                        sessionError = sessionError,
                        sessionState = sessionState,
                        onCancelConnect = {
                            scope.launch(Dispatchers.IO) { WeaveloClient.disconnect() }
                        },
                        onConnectHost = { deviceId, endpoint ->
                            scope.launch(Dispatchers.IO) { WeaveloClient.connect(deviceId, endpoint) }
                        },
                        onConnectWorkspace = { deviceId, endpoint -> sendFileOnConnect = false; workspaceDeviceId = deviceId; workspaceEndpoint = endpoint },
                        onSendFiles = { deviceId, endpoint -> sendFileOnConnect = true; workspaceDeviceId = deviceId; workspaceEndpoint = endpoint },
                        publishing = PublisherClient.active,
                        receivingFiles = receiverState.active || receiverState.starting,
                        receiverStarting = receiverState.starting,
                        onStartReceiving = { FileReceiverClient.start(this) },
                        onStopReceiving = { FileReceiverClient.stop(this) },
                        onShareScreen = { audio ->
                            if (audio && checkSelfPermission(android.Manifest.permission.RECORD_AUDIO) != android.content.pm.PackageManager.PERMISSION_GRANTED) audioPermission.launch(android.Manifest.permission.RECORD_AUDIO)
                            else requestProjection(audio)
                        },
                        onStopSharing = { PublisherClient.stop(this) },
                        onBrowseFiles = { receivedFiles = true },
                        onJoinCode = ::joinNetwork,
                        onScanQr = { scanningQr = true }
                    )
                }
            }
            if (receivedFiles) `fun`.hackerlife.weavelo.ui.ReceivedFilesDialog { receivedFiles = false }
        }
    }
}
