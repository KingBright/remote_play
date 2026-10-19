package `fun`.hackerlife.weavelo.ui

import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.graphics.Bitmap
import android.os.Build
import android.os.PersistableBundle
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.google.zxing.common.BitMatrix
import `fun`.hackerlife.weavelo.DeviceInvitation
import `fun`.hackerlife.weavelo.WeaveloClient
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlin.math.floor

@Composable
fun InvitationDialog(onDismiss: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var invitation by remember { mutableStateOf<DeviceInvitation?>(null) }
    var matrix by remember { mutableStateOf<BitMatrix?>(null) }
    var message by remember { mutableStateOf("Loading invitation…") }
    var saving by remember { mutableStateOf(false) }
    LaunchedEffect(Unit) {
        try {
            val (data, qr) = withContext(Dispatchers.IO) {
                val data = WeaveloClient.currentInvitation()
                data to invitationQr(data.qrPayload)
            }
            invitation = data
            matrix = qr
            message = "Scan with Weavelo on another phone, or save the PNG and open it with Scan QR Image on desktop."
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (_: Exception) { message = "Could not load the existing invitation. Close and try again with the matching app update." }
    }
    val saveImage = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("image/png")) { uri ->
        val qr = matrix
        if (uri != null && qr != null && !saving) {
            saving = true
            scope.launch {
                try {
                    withContext(Dispatchers.IO) {
                        val side = qr.width * 8
                        val pixels = IntArray(side * side) { i -> if (qr[i % side / 8, i / side / 8]) android.graphics.Color.BLACK else android.graphics.Color.WHITE }
                        val bitmap = Bitmap.createBitmap(pixels, side, side, Bitmap.Config.ARGB_8888)
                        try {
                            val output = context.contentResolver.openOutputStream(uri) ?: error("Image destination unavailable")
                            output.use { check(bitmap.compress(Bitmap.CompressFormat.PNG, 100, it)) }
                        } finally { bitmap.recycle() }
                    }
                    message = "QR image saved. Transfer it yourself to the device you want to join."
                } catch (cancelled: CancellationException) { throw cancelled }
                catch (_: Exception) { message = "Could not save the QR image. Choose another location and try again." }
                finally { saving = false }
            }
        }
    }
    AlertDialog(onDismissRequest = onDismiss,
        title = { Text("Invite a device") },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                invitation?.let { Text(it.networkName) }
                matrix?.let { qr ->
                    Canvas(Modifier.fillMaxWidth().aspectRatio(1f).background(Color.White)
                        .semantics { contentDescription = "Invitation QR for the current device group" }) {
                        val cell = floor(size.minDimension / qr.width).coerceAtLeast(1f)
                        val left = floor((size.width - cell * qr.width) / 2)
                        val top = floor((size.height - cell * qr.height) / 2)
                        for (y in 0 until qr.height) for (x in 0 until qr.width) if (qr[x, y]) {
                            drawRect(Color.Black, Offset(left + x * cell, top + y * cell), Size(cell, cell))
                        }
                    }
                }
                Text(message)
                Row {
                    TextButton(enabled = matrix != null && !saving,
                        onClick = { saveImage.launch("Weavelo-invitation.png") }) { Text(if (saving) "Saving…" else "Save PNG") }
                    TextButton(enabled = invitation != null, onClick = {
                        invitation?.let { data ->
                            val clip = ClipData.newPlainText("Weavelo invitation", data.code)
                            if (Build.VERSION.SDK_INT >= 33) clip.description.extras = PersistableBundle().apply {
                                putBoolean(ClipDescription.EXTRA_IS_SENSITIVE, true)
                            }
                            context.getSystemService(ClipboardManager::class.java).setPrimaryClip(clip)
                            message = "Invitation code copied."
                        }
                    }) { Text("Copy code") }
                }
            }
        },
        confirmButton = { TextButton(onClick = onDismiss) { Text("Close") } })
}
