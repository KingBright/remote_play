package com.remoteplay.client.ui

import androidx.compose.ui.geometry.Offset
import kotlin.math.abs

enum class TwoFingerGestureMode {
    UNDECIDED,
    REMOTE_GESTURE,
    LOCAL_TRANSFORM
}

/**
 * Keep the established two-finger remote gestures at 1x. A gesture becomes a
 * local viewport transform only after a real pinch is observed, or whenever
 * the viewport is already zoomed. Translation at 1x remains remote scrolling.
 */
fun classifyTwoFingerGesture(
    currentScale: Float,
    initialDistance: Float,
    currentDistance: Float,
    initialCentroid: Offset,
    currentCentroid: Offset,
    pinchRatioThreshold: Float = 0.035f,
    panThresholdPx: Float = 8f
): TwoFingerGestureMode {
    if (currentScale > 1.01f) return TwoFingerGestureMode.LOCAL_TRANSFORM
    val baseline = initialDistance.coerceAtLeast(1f)
    val distanceRatio = abs(currentDistance - initialDistance) / baseline
    if (distanceRatio >= pinchRatioThreshold) return TwoFingerGestureMode.LOCAL_TRANSFORM
    if ((currentCentroid - initialCentroid).getDistance() >= panThresholdPx) {
        return TwoFingerGestureMode.REMOTE_GESTURE
    }
    return TwoFingerGestureMode.UNDECIDED
}

/**
 * 视口缩放与平移状态机及坐标逆映射计算。
 * 支持 1.0x ~ 3.5x 无级缩放、双指捏合平移、边界限制与精准坐标逆映射。
 */
data class ViewportTransform(
    val scale: Float = 1.0f,
    val offset: Offset = Offset.Zero,
    val minScale: Float = 1.0f,
    val maxScale: Float = 3.5f
) {
    /**
     * 计算平移边界限制：确保局部放大状态下视口始终覆盖在内容有效区域内。
     * 最大偏移量由 (viewSize * (scale - 1)) / 2 决定。
     */
    fun clampOffset(proposedOffset: Offset, viewWidth: Float, viewHeight: Float, currentScale: Float = scale): Offset {
        if (currentScale <= 1.0f) {
            return Offset.Zero
        }
        val maxOffsetX = ((viewWidth * (currentScale - 1.0f)) / 2f).coerceAtLeast(0f)
        val maxOffsetY = ((viewHeight * (currentScale - 1.0f)) / 2f).coerceAtLeast(0f)
        val clampedX = proposedOffset.x.coerceIn(-maxOffsetX, maxOffsetX)
        val clampedY = proposedOffset.y.coerceIn(-maxOffsetY, maxOffsetY)
        return Offset(clampedX, clampedY)
    }

    /**
     * 缩放和平移更新：支持以捏合焦点 (focalPoint) 为中心进行无漂移缩放与平移
     */
    fun update(
        zoomFactor: Float,
        panDelta: Offset,
        viewWidth: Float,
        viewHeight: Float,
        focalPoint: Offset = Offset(viewWidth / 2f, viewHeight / 2f)
    ): ViewportTransform {
        val newScale = (scale * zoomFactor).coerceIn(minScale, maxScale)
        val cx = viewWidth / 2f
        val cy = viewHeight / 2f
        val scaleRatio = if (scale > 0.001f) newScale / scale else 1.0f

        // 以 focalPoint 为缩放中心：
        // 新偏移量 = (focalPoint + panDelta) - center - (focalPoint - center - offset) * scaleRatio
        val proposedX = (focalPoint.x + panDelta.x) - cx - (focalPoint.x - cx - offset.x) * scaleRatio
        val proposedY = (focalPoint.y + panDelta.y) - cy - (focalPoint.y - cy - offset.y) * scaleRatio

        val newOffset = clampOffset(Offset(proposedX, proposedY), viewWidth, viewHeight, newScale)
        return copy(scale = newScale, offset = newOffset)
    }

    /**
     * 快速复位至 1.0x
     */
    fun reset(): ViewportTransform = copy(scale = 1.0f, offset = Offset.Zero)

    fun reclamp(viewWidth: Float, viewHeight: Float): ViewportTransform =
        copy(offset = clampOffset(offset, viewWidth, viewHeight))

    /** Preserve the normalized content at the viewport center across layout changes. */
    fun resized(
        previousWidth: Float,
        previousHeight: Float,
        viewWidth: Float,
        viewHeight: Float
    ): ViewportTransform {
        if (previousWidth <= 0f || previousHeight <= 0f) return reclamp(viewWidth, viewHeight)
        val scaledOffset = Offset(
            offset.x * viewWidth / previousWidth,
            offset.y * viewHeight / previousHeight
        )
        return copy(offset = clampOffset(scaledOffset, viewWidth, viewHeight))
    }

    /**
     * 精准逆映射：将屏幕触控绝对坐标映射回内容空间，再换算为发送给 JNI 的归一化坐标 [0f, 1f]。
     * 逆变换公式：
     * contentX = cx + (screenX - cx - offset.x) / scale
     * contentY = cy + (screenY - cy - offset.y) / scale
     */
    fun screenToNormalized(
        screenX: Float,
        screenY: Float,
        viewWidth: Float,
        viewHeight: Float
    ): Pair<Float, Float> {
        val w = viewWidth.coerceAtLeast(1f)
        val h = viewHeight.coerceAtLeast(1f)
        val cx = w / 2f
        val cy = h / 2f
        val s = scale.coerceAtLeast(0.01f)

        val contentX = cx + (screenX - cx - offset.x) / s
        val contentY = cy + (screenY - cy - offset.y) / s

        val normX = (contentX / w).coerceIn(0f, 1f)
        val normY = (contentY / h).coerceIn(0f, 1f)
        return Pair(normX, normY)
    }

    /**
     * 正向映射（主要用于单测验证零漂移可逆性）：将内容空间归一化坐标映射至屏幕绝对像素坐标。
     */
    fun normalizedToScreen(
        normX: Float,
        normY: Float,
        viewWidth: Float,
        viewHeight: Float
    ): Pair<Float, Float> {
        val w = viewWidth.coerceAtLeast(1f)
        val h = viewHeight.coerceAtLeast(1f)
        val cx = w / 2f
        val cy = h / 2f
        val contentX = normX * w
        val contentY = normY * h

        val screenX = cx + (contentX - cx) * scale + offset.x
        val screenY = cy + (contentY - cy) * scale + offset.y
        return Pair(screenX, screenY)
    }
}
