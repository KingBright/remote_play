package com.remoteplay.client.ui

import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.runtime.MutableState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.composed
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChanged
import androidx.compose.ui.unit.IntSize
import com.remoteplay.client.TouchMode

/** The production gesture handler, with an injectable transport sink for on-device tests. */
internal fun Modifier.remoteViewportGestures(
    transformState: MutableState<ViewportTransform>,
    touchMode: TouchMode,
    viewportSize: IntSize,
    enabled: Boolean,
    contentKey: Any? = null,
    sendTouch: (Int, Int, Float, Float, Float) -> Unit
): Modifier = composed {
    val currentSink by rememberUpdatedState(sendTouch)
    pointerInput(touchMode, viewportSize, enabled, contentKey) {
        var transform by transformState
        // At 1x, two-finger drag/tap remains a remote scroll/right-click.
        // A real pinch takes ownership locally and cancels remote pointers.
        awaitEachGesture {
            val activePointers = mutableSetOf<Int>()
            var twoFingerMode = TwoFingerGestureMode.UNDECIDED
            var twoFingerStarted = false
            var localTransformGesture = false
            var initialDistance = 0f
            var initialCentroid = Offset.Zero
            var prevDistance = 0f
            var prevCentroid = Offset.Zero
            var pairIds = emptySet<Long>()
            var suppressGesture = false

            fun forwardRemoteChanges(
                changes: List<androidx.compose.ui.input.pointer.PointerInputChange>,
                viewWidth: Float,
                viewHeight: Float
            ) {
                // Paused/disconnected/modal viewing may still zoom locally, but
                // must never issue commands against an old remote frame.
                if (!enabled) {
                    for (change in changes) change.consume()
                    return
                }
                for (change in changes) {
                    val pointerId = change.id.value.toInt()
                    val (normX, normY) = if (touchMode == TouchMode.DIRECT) {
                        transform.screenToNormalized(change.position.x, change.position.y, viewWidth, viewHeight)
                    } else {
                        // Trackpad deltas must not shrink as the viewport zooms.
                        (change.position.x / viewWidth).coerceIn(0f, 1f) to
                            (change.position.y / viewHeight).coerceIn(0f, 1f)
                    }
                    val pressure = if (change.pressed) 1f else 0f
                    when {
                        !change.previousPressed && change.pressed -> {
                            activePointers.add(pointerId)
                            currentSink(
                                0, pointerId, normX, normY, pressure
                            )
                        }
                        change.previousPressed && !change.pressed && activePointers.contains(pointerId) -> {
                            activePointers.remove(pointerId)
                            currentSink(
                                2, pointerId, normX, normY, pressure
                            )
                        }
                        change.pressed && change.positionChanged() && activePointers.contains(pointerId) -> {
                            activePointers.add(pointerId)
                            currentSink(
                                1, pointerId, normX, normY, pressure
                            )
                        }
                    }
                    change.consume()
                }
            }

            fun cancelRemotePointers() {
                for (id in activePointers) {
                    currentSink(3, id, 0f, 0f, 0f)
                }
                activePointers.clear()
            }

            try {
                while (true) {
                    val event = awaitPointerEvent()
                    // Child controls (including RESET) own their input. Never
                    // forward a consumed local tap to the remote computer.
                    if (event.changes.any { it.isConsumed }) {
                        cancelRemotePointers()
                        break
                    }
                    val viewWidth = size.width.coerceAtLeast(1).toFloat()
                    val viewHeight = size.height.coerceAtLeast(1).toFloat()
                    val pressedChanges = event.changes.filter { it.pressed }

                    if (pressedChanges.size > 2) {
                        suppressGesture = true
                        cancelRemotePointers()
                    }
                    if (suppressGesture) {
                        for (change in event.changes) change.consume()
                    } else if (pressedChanges.size == 2) {
                        val p0 = pressedChanges[0].position
                        val p1 = pressedChanges[1].position
                        val currentDistance = (p0 - p1).getDistance()
                        val currentCentroid = (p0 + p1) / 2f

                        val currentPair = pressedChanges.map { it.id.value }.toSet()
                        if (!twoFingerStarted || pairIds != currentPair) {
                            pairIds = currentPair
                            twoFingerStarted = true
                            initialDistance = currentDistance
                            initialCentroid = currentCentroid
                            prevDistance = currentDistance
                            prevCentroid = currentCentroid
                            twoFingerMode = classifyTwoFingerGesture(
                                transform.scale,
                                initialDistance,
                                currentDistance,
                                initialCentroid,
                                currentCentroid
                            )
                        } else if (twoFingerMode == TwoFingerGestureMode.UNDECIDED) {
                            twoFingerMode = classifyTwoFingerGesture(
                                transform.scale,
                                initialDistance,
                                currentDistance,
                                initialCentroid,
                                currentCentroid
                            )
                        }

                        if (twoFingerMode == TwoFingerGestureMode.LOCAL_TRANSFORM) {
                            val baselineDistance = if (localTransformGesture) prevDistance else initialDistance
                            val baselineCentroid = if (localTransformGesture) prevCentroid else initialCentroid
                            if (!localTransformGesture) {
                                localTransformGesture = true
                                cancelRemotePointers()
                            }
                            val zoom = if (baselineDistance > 1f) currentDistance / baselineDistance else 1f
                            val pan = currentCentroid - baselineCentroid
                            transform = transform.update(
                                zoom,
                                pan,
                                viewWidth,
                                viewHeight,
                                baselineCentroid
                            )
                            for (change in event.changes) change.consume()
                        } else {
                            // Buffer movement while classifying. Forward only
                            // pointer edges so a pinch cannot scroll remotely
                            // before it crosses the local pinch threshold.
                            val changes = if (twoFingerMode == TwoFingerGestureMode.UNDECIDED) {
                                event.changes.filter { it.pressed != it.previousPressed }
                            } else event.changes
                            forwardRemoteChanges(changes, viewWidth, viewHeight)
                            for (change in event.changes) change.consume()
                        }
                        prevDistance = currentDistance
                        prevCentroid = currentCentroid
                    } else if (localTransformGesture) {
                        // Once a pinch owns the gesture, swallow the staggered
                        // finger-up tail so it cannot become a remote click.
                        for (change in event.changes) change.consume()
                    } else {
                        forwardRemoteChanges(event.changes, viewWidth, viewHeight)
                    }

                    if (event.changes.none { it.pressed }) break
                }
            } finally {
                cancelRemotePointers()
            }
        }
    }
}
