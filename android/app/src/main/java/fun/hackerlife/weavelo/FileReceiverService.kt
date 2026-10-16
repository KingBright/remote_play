package `fun`.hackerlife.weavelo

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.os.IBinder
import kotlinx.coroutines.*

/** User-started, visible file receiver; no MediaProjection or microphone access. */
class FileReceiverService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val token = Any()
    private var worker: Job? = null

    override fun onCreate() {
        super.onCreate()
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(NotificationChannel("file-receiver", "Receive files", NotificationManager.IMPORTANCE_LOW))
        val open = PendingIntent.getActivity(this, 5, Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        val stop = PendingIntent.getService(this, 6, Intent(this, FileReceiverService::class.java).setAction("stop"),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        startForeground(8125, Notification.Builder(this, "file-receiver")
            .setSmallIcon(android.R.drawable.stat_sys_download)
            .setContentTitle("Weavelo file reception")
            .setContentText("Device-network members can send files. No screen permission is granted.")
            .setContentIntent(open).setOngoing(true)
            .addAction(Notification.Action.Builder(null, "Stop standalone receiver", stop).build()).build())
    }
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == "stop") { stopSelf(); return START_NOT_STICKY }
        if (worker == null) worker = scope.launch {
            var failure: String? = null
            try {
                FileReceiverClient.acquire(applicationContext, token)
                while (isActive) { delay(2000); FileReceiverClient.checkHealth(token) }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                failure = error.message ?: "File reception failed"
            } finally {
                withContext(NonCancellable) { FileReceiverClient.release(token, failure) }
                stopSelf()
            }
        }
        return START_NOT_STICKY
    }
    override fun onDestroy() { scope.cancel(); super.onDestroy() }
    override fun onBind(intent: Intent?): IBinder? = null
}
