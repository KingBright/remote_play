package `fun`.hackerlife.weavelo.ui

import android.Manifest
import android.content.pm.PackageManager
import android.util.Size
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean

@Composable
fun QrScanScreen(
    onResult: (String) -> Unit,
    onCancel: () -> Unit,
    onManualInput: () -> Unit
) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val currentResult by rememberUpdatedState(onResult)
    val session = remember { QrScanSession() }
    var permissionGranted by remember {
        mutableStateOf(ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED)
    }
    var cameraMessage by remember { mutableStateOf<String?>(null) }
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (session.isActive) {
            permissionGranted = granted
            if (!granted) cameraMessage = "Camera access was not granted. You can enter an invitation instead."
        }
    }
    LaunchedEffect(Unit) { if (!permissionGranted) permission.launch(Manifest.permission.CAMERA) }
    DisposableEffect(session) { onDispose { session.close() } }
    val cancel = { session.close(); onCancel() }
    BackHandler(onBack = cancel)

    val previewView = remember(context) { PreviewView(context) }
    if (permissionGranted) {
        DisposableEffect(previewView, lifecycleOwner) {
            val disposed = AtomicBoolean(false)
            val executor = Executors.newSingleThreadExecutor()
            val mainExecutor = ContextCompat.getMainExecutor(context)
            val decoder = QrFrameDecoder()
            val preview = Preview.Builder().build().also { it.setSurfaceProvider(previewView.surfaceProvider) }
            val analysis = ImageAnalysis.Builder()
                .setTargetResolution(Size(1280, 720))
                .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                .build()
            var provider: ProcessCameraProvider? = null
            analysis.setAnalyzer(executor) { image ->
                try {
                    if (!disposed.get() && session.acceptsFrames) {
                        val plane = image.planes.firstOrNull()
                        val crop = image.cropRect
                        val text = plane?.let {
                            decoder.decode(it.buffer, image.width, image.height, it.rowStride, it.pixelStride,
                                crop.left, crop.top, crop.width(), crop.height())
                        }
                        if (text != null && session.claimResult()) {
                            mainExecutor.execute {
                                if (!disposed.get() && session.isActive) currentResult(text)
                            }
                        }
                    }
                } finally {
                    image.close()
                }
            }
            val future = ProcessCameraProvider.getInstance(context)
            future.addListener({
                if (!disposed.get() && session.isActive) {
                    try {
                        val cameraProvider = future.get()
                        provider = cameraProvider
                        if (cameraProvider.hasCamera(CameraSelector.DEFAULT_BACK_CAMERA)) {
                            cameraProvider.bindToLifecycle(lifecycleOwner, CameraSelector.DEFAULT_BACK_CAMERA, preview, analysis)
                        } else {
                            cameraMessage = "No rear camera is available. Enter an invitation to join."
                        }
                    } catch (_: Exception) {
                        cameraMessage = "The camera could not start. Try scanning again or enter an invitation."
                    }
                }
            }, mainExecutor)
            onDispose {
                disposed.set(true)
                analysis.clearAnalyzer()
                // Only release this screen's use cases, never another camera owner.
                provider?.unbind(preview, analysis)
                executor.shutdown()
            }
        }
    }

    Box(Modifier.fillMaxSize().background(ColorBaseBg)) {
        if (permissionGranted && cameraMessage == null) {
            AndroidView(modifier = Modifier.fillMaxSize(), factory = { previewView })
        }
        Column(Modifier.align(Alignment.BottomCenter).fillMaxWidth()
            .background(ColorBaseBg.copy(alpha = 0.92f)).safeDrawingPadding().padding(20.dp),
            horizontalAlignment = Alignment.CenterHorizontally) {
            Text(cameraMessage ?: "Scan the invitation QR shown in Weavelo on your other device.",
                color = ColorTextPrimary, fontSize = 14.sp)
            Row(horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                TextButton(onClick = { session.close(); onManualInput() }) { Text("Enter invitation", color = ColorAccentCyan) }
                TextButton(onClick = cancel) { Text("Cancel", color = ColorTextPrimary) }
            }
        }
    }
}
