package com.remoteplay.client

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.os.IBinder
import kotlinx.coroutines.*

/** Keeps an explicitly connected remote-device session alive while media is paused. */
class SessionConnectionService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    override fun onCreate() {
        super.onCreate()
        val manager = getSystemService(NotificationManager::class.java)
        val channel = "remote-session"
        manager.createNotificationChannel(NotificationChannel(channel, "Remote connection", NotificationManager.IMPORTANCE_LOW))
        val open = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        val stop = PendingIntent.getService(this, 1, Intent(this, SessionConnectionService::class.java).setAction("disconnect"), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        val builder = Notification.Builder(this, channel)
        startForeground(8123, builder
            .setSmallIcon(android.R.drawable.stat_sys_upload_done)
            .setContentTitle("RemotePlay connected")
            .setContentText("Background media pauses automatically. Tap to return.")
            .setContentIntent(open).setOngoing(true)
            .addAction(Notification.Action.Builder(null, "Disconnect", stop).build())
            .build())
        scope.launch {
            while (isActive) {
                if (RemotePlayClient.backgrounded) {
                    if (!RemotePlayClient.maintainBackgroundConnection()) RemotePlayClient.pollTelemetry()
                }
                if (!WorkspaceClient.retainingConnection && RemotePlayClient.sessionState.value in listOf(SessionState.DISCONNECTED, SessionState.ERROR)) {
                    stopSelf()
                    break
                }
                delay(if (RemotePlayClient.backgrounded) 5000 else 1000)
            }
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == "disconnect") scope.launch {
            RemotePlayClient.disconnect()
            WorkspaceClient.close()
            stopSelf()
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        scope.cancel()
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null
}
