package com.remoteplay.client

import android.content.Context
import android.content.Intent
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import org.json.JSONObject

object PublisherClient {
    external fun nativeAction(operation: Int, payload: String): String
    external fun nativeFrame(bytes: ByteArray, ptsUs: Long)
    external fun nativePcm(samples: ShortArray): Boolean
    private val mutable = MutableStateFlow("")
    val status = mutable.asStateFlow()
    @Volatile var active = false
        private set
    fun status(message: String, active: Boolean = this.active) { this.active = active; mutable.value = message }
    fun action(operation: Int, payload: String = ""): String {
        val result = nativeAction(operation, payload)
        if (result.startsWith("{\"error\"")) throw IllegalStateException(JSONObject(result).getString("error"))
        return result
    }
    fun start(context: Context, resultCode: Int, data: Intent, audio: Boolean) {
        context.startForegroundService(Intent(context, ProjectionService::class.java)
            .putExtra("result", resultCode).putExtra("consent", data).putExtra("audio", audio))
    }
    fun stop(context: Context) { context.stopService(Intent(context, ProjectionService::class.java)) }
}
