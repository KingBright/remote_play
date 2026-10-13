package com.remoteplay.client.ui

import android.graphics.Bitmap
import android.graphics.Paint
import android.view.SurfaceHolder
import android.view.SurfaceView
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.boundsInWindow
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.test.platform.app.InstrumentationRegistry
import com.remoteplay.client.TouchMode
import java.io.File
import java.util.concurrent.CopyOnWriteArrayList
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

/** Offline Android device tests. No RemotePlay connection, JNI, microphone or desktop capture. */
class RemoteViewportInteractionTest {
    @get:Rule val compose = createComposeRule()
    private data class Touch(val action: Int, val id: Int, val x: Float, val y: Float)
    private val touches = CopyOnWriteArrayList<Touch>()
    private val transform = mutableStateOf(ViewportTransform())
    private val enabled = mutableStateOf(true)
    private val mode = mutableStateOf(TouchMode.DIRECT)
    private val aspect = mutableStateOf(16f / 9f)
    private val contentRevision = mutableStateOf(0)
    private val size = mutableStateOf(IntSize.Zero)
    @Volatile private var bounds = Rect.Zero
    @Volatile private var surfaceDrawn = false

    private fun showViewport() {
        compose.setContent {
            MaterialTheme {
                BoxWithConstraints(Modifier.fillMaxSize().background(Color.Black).testTag("root")) {
                    val viewportWidth = minOf(maxWidth, maxHeight * aspect.value)
                    Box(
                        Modifier.align(Alignment.Center).width(viewportWidth)
                            .aspectRatio(aspect.value).clipToBounds()
                            .onGloballyPositioned { bounds = it.boundsInWindow() }
                            .onSizeChanged {
                                val previousSize = size.value
                                size.value = it
                                transform.value = transform.value.resized(previousSize.width.toFloat(), previousSize.height.toFloat(), it.width.toFloat(), it.height.toFloat())
                            }
                            .remoteViewportGestures(transform, mode.value, size.value, enabled.value, contentKey = contentRevision.value) {
                                action, id, x, y, _ -> touches.add(Touch(action, id, x, y))
                            }.testTag("viewport")
                    ) {
                        RemoteVideoSurface(transform.value, Modifier.fillMaxSize()) { context ->
                            SurfaceView(context).apply {
                                holder.addCallback(object : SurfaceHolder.Callback {
                                    override fun surfaceCreated(holder: SurfaceHolder) = drawFixture(holder)
                                    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) = drawFixture(holder)
                                    override fun surfaceDestroyed(holder: SurfaceHolder) { surfaceDrawn = false }
                                })
                            }
                        }
                        Text("RESET", Modifier.align(Alignment.BottomEnd)
                            .background(Color.DarkGray).clickable { transform.value = transform.value.reset() }
                            .padding(12.dp).testTag("reset"), color = Color.White)
                    }
                }
            }
        }
        compose.waitUntil(20_000) { size.value.width > 0 && surfaceDrawn }
        compose.waitForIdle()
    }

    private fun drawFixture(holder: SurfaceHolder) {
        val canvas = holder.lockCanvas() ?: return
        try {
            canvas.drawColor(android.graphics.Color.rgb(30, 38, 46))
            val paint = Paint().apply { color = android.graphics.Color.rgb(68, 90, 105); strokeWidth = 2f }
            for (n in 1..9) {
                val x = canvas.width * n / 10f
                val y = canvas.height * n / 10f
                canvas.drawLine(x, 0f, x, canvas.height.toFloat(), paint)
                canvas.drawLine(0f, y, canvas.width.toFloat(), y, paint)
            }
            paint.color = android.graphics.Color.MAGENTA
            canvas.drawCircle(canvas.width * .65f, canvas.height * .6f, 9f, paint)
        } finally { holder.unlockCanvasAndPost(canvas) }
        surfaceDrawn = true
    }

    private fun viewport() = compose.onNodeWithTag("viewport")
    private fun screenshot(name: String): Bitmap {
        compose.waitForIdle()
        // Surface composition is asynchronous to Compose idleness.
        Thread.sleep(300)
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val bitmap = instrumentation.uiAutomation.takeScreenshot() ?: error("Screenshot unavailable")
        val dir = File(instrumentation.targetContext.getExternalFilesDir(null), "viewport-tests").apply { mkdirs() }
        File(dir, "$name.png").outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
        return bitmap
    }

    private fun marker(bitmap: Bitmap): Offset {
        var sumX = 0L; var sumY = 0L; var count = 0
        for (y in 0 until bitmap.height) for (x in 0 until bitmap.width) {
            val c = bitmap.getPixel(x, y)
            if (android.graphics.Color.red(c) > 210 && android.graphics.Color.blue(c) > 210 && android.graphics.Color.green(c) < 70) {
                sumX += x; sumY += y; count++
            }
        }
        assertTrue("Rendered SurfaceView marker is missing", count > 20)
        return Offset(sumX.toFloat() / count, sumY.toFloat() / count)
    }

    @Test fun surface_pixels_and_remote_touch_share_the_same_zoom_transform() {
        showViewport()
        val first = screenshot("surface-1x")
        val initial = marker(first)
        assertEquals(bounds.left + bounds.width * .65f, initial.x, 5f)
        assertEquals(bounds.top + bounds.height * .6f, initial.y, 5f)
        compose.runOnIdle {
            transform.value = ViewportTransform(2f, Offset(-size.value.width * .12f, size.value.height * .03f))
        }
        val rendered = screenshot("surface-2x")
        val actual = marker(rendered)
        val margin = rendered.getPixel(bounds.center.x.toInt(), (bounds.top - 16).toInt())
        assertTrue("Zoomed SurfaceView escaped its viewport clipping",
            android.graphics.Color.red(margin) < 8 && android.graphics.Color.green(margin) < 8 && android.graphics.Color.blue(margin) < 8)
        val expected = transform.value.normalizedToScreen(.65f, .6f, bounds.width, bounds.height)
        assertEquals("Rendered X differs from the input transform", bounds.left + expected.first, actual.x, 6f)
        assertEquals("Rendered Y differs from the input transform", bounds.top + expected.second, actual.y, 6f)
        viewport().performTouchInput { click(Offset(actual.x - bounds.left, actual.y - bounds.top)) }
        compose.runOnIdle {
            val up = touches.last { it.action == 2 }
            assertEquals(.65f, up.x, .006f)
            assertEquals(.6f, up.y, .006f)
        }
        first.recycle(); rendered.recycle()
    }

    @Test fun pinch_cancels_remote_touches_and_swallows_staggered_finger_up() {
        showViewport()
        viewport().performTouchInput {
            down(0, Offset(width * .4f, height * .5f)); down(1, Offset(width * .6f, height * .5f))
            moveTo(0, Offset(width * .25f, height * .5f)); moveTo(1, Offset(width * .75f, height * .5f))
            up(0); advanceEventTime(40); moveTo(1, Offset(width * .78f, height * .5f)); up(1)
        }
        compose.runOnIdle {
            assertTrue(transform.value.scale > 2f)
            assertTrue(touches.any { it.action == 3 })
            assertFalse("Local pinch leaked remote movement/click", touches.any { it.action == 1 || it.action == 2 })
            touches.clear()
        }
        viewport().performTouchInput { click(center) }
        compose.runOnIdle { assertEquals(listOf(0, 2), touches.map { it.action }) }
    }

    @Test fun one_x_parallel_two_finger_drag_stays_remote() {
        showViewport()
        viewport().performTouchInput {
            down(0, Offset(width * .35f, height * .4f)); down(1, Offset(width * .65f, height * .4f))
            updatePointerTo(0, Offset(width * .35f, height * .55f))
            updatePointerTo(1, Offset(width * .65f, height * .55f))
            move()
            up(0); up(1)
        }
        compose.runOnIdle { assertEquals(1f, transform.value.scale, .001f); assertTrue(touches.any { it.action == 1 }) }
    }

    @Test fun paused_or_modal_viewport_does_not_forward_remote_touches() {
        enabled.value = false
        showViewport()
        viewport().performTouchInput { click(center) }
        compose.runOnIdle { assertTrue("Paused viewport forwarded touch input", touches.isEmpty()) }
    }

    @Test fun pausing_during_a_drag_sends_cancel_but_not_a_late_click() {
        showViewport()
        viewport().performTouchInput { down(center); moveTo(Offset(width * .65f, height * .5f)) }
        compose.runOnIdle { enabled.value = false }
        compose.waitForIdle()
        viewport().performTouchInput { up() }
        compose.runOnIdle { assertTrue(touches.any { it.action == 3 }); assertFalse(touches.any { it.action == 2 }) }
    }

    @Test fun reset_control_is_local_and_never_leaks_to_remote() {
        showViewport()
        compose.runOnIdle { transform.value = ViewportTransform(2f) }
        compose.onNodeWithTag("reset").performTouchInput { click(center) }
        compose.runOnIdle { assertEquals(1f, transform.value.scale, .001f); assertTrue(touches.isEmpty()) }
    }

    @Test fun three_finger_gestures_are_cancelled_without_a_click_tail() {
        showViewport()
        viewport().performTouchInput {
            down(0, Offset(width * .3f, height * .5f)); down(1, center); down(2, Offset(width * .7f, height * .5f))
            up(0); up(1); up(2)
        }
        compose.runOnIdle { assertTrue(touches.any { it.action == 3 }); assertFalse(touches.any { it.action == 2 }) }
    }

    @Test fun viewport_resize_reclamps_zoom_and_cancels_active_input() {
        showViewport()
        compose.runOnIdle { transform.value = ViewportTransform(2f, Offset(size.value.width / 2f, size.value.height / 2f)) }
        viewport().performTouchInput { down(center) }
        compose.runOnIdle { aspect.value = 9f / 16f }
        compose.waitForIdle()
        viewport().performTouchInput { up() }
        compose.runOnIdle {
            assertTrue(touches.any { it.action == 3 })
            assertTrue(kotlin.math.abs(transform.value.offset.x) <= size.value.width / 2f + 1)
            assertTrue(kotlin.math.abs(transform.value.offset.y) <= size.value.height / 2f + 1)
        }
        screenshot("surface-portrait").recycle()
    }

    @Test fun black_bars_do_not_send_remote_input() {
        showViewport()
        compose.onNodeWithTag("root").performTouchInput { click(Offset(15f, 15f)) }
        compose.runOnIdle { assertTrue(touches.isEmpty()) }
    }

    @Test fun resizing_keeps_the_same_content_at_the_viewport_center() {
        showViewport()
        compose.runOnIdle { transform.value = ViewportTransform(2f, Offset(size.value.width * .15f, size.value.height * .2f)) }
        val before = transform.value.screenToNormalized(size.value.width / 2f, size.value.height / 2f, size.value.width.toFloat(), size.value.height.toFloat())
        compose.runOnIdle { aspect.value = 9f / 16f }
        compose.waitForIdle()
        val after = transform.value.screenToNormalized(size.value.width / 2f, size.value.height / 2f, size.value.width.toFloat(), size.value.height.toFloat())
        assertEquals("Viewport resize shifted the viewed content horizontally", before.first, after.first, .002f)
        assertEquals("Viewport resize shifted the viewed content vertically", before.second, after.second, .002f)
    }

    @Test fun paused_video_still_allows_local_zoom_without_remote_input() {
        enabled.value = false
        showViewport()
        viewport().performTouchInput {
            down(0, Offset(width * .4f, height * .5f)); down(1, Offset(width * .6f, height * .5f))
            moveTo(0, Offset(width * .25f, height * .5f)); moveTo(1, Offset(width * .75f, height * .5f))
            up(0); up(1)
        }
        compose.runOnIdle { assertTrue(transform.value.scale > 2f); assertTrue(touches.isEmpty()) }
    }

    @Test fun remote_double_click_does_not_change_local_zoom() {
        showViewport()
        compose.runOnIdle { transform.value = ViewportTransform(2f) }
        viewport().performTouchInput { doubleClick(center) }
        compose.runOnIdle { assertEquals(2f, transform.value.scale, .001f); assertEquals(2, touches.count { it.action == 2 }) }
    }

    @Test fun content_change_cancels_a_drag_even_when_the_viewport_size_is_unchanged() {
        showViewport()
        viewport().performTouchInput { down(center); moveTo(Offset(width * .6f, height * .5f)) }
        compose.runOnIdle { contentRevision.value += 1 }
        compose.waitForIdle()
        viewport().performTouchInput { up() }
        compose.runOnIdle {
            assertTrue(touches.any { it.action == 3 })
            assertFalse("A stale touch-up crossed the content change", touches.any { it.action == 2 })
        }
    }

    @Test fun zoom_does_not_reduce_trackpad_sensitivity() {
        mode.value = TouchMode.TRACKPAD
        showViewport()
        compose.runOnIdle { transform.value = ViewportTransform(3f) }
        viewport().performTouchInput { down(center); moveTo(Offset(width * .6f, height * .5f)); up() }
        compose.runOnIdle {
            assertEquals(.1f, touches.last { it.action == 1 }.x - touches.first { it.action == 0 }.x, .003f)
        }
    }
}
