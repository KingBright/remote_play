package com.remoteplay.client.ui

import android.content.Context
import android.view.SurfaceView
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.viewinterop.AndroidView

/** Production video surface composition, also exercised by the offline instrumentation fixture. */
@Composable
internal fun RemoteVideoSurface(
    transform: ViewportTransform,
    modifier: Modifier = Modifier,
    factory: (Context) -> SurfaceView
) {
    AndroidView(
        modifier = modifier.graphicsLayer {
            scaleX = transform.scale
            scaleY = transform.scale
            translationX = transform.offset.x
            translationY = transform.offset.y
        },
        factory = factory
    )
}
