package dev.flipflop.ui

import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.input.pointer.PointerEvent

/** Handles the system Back button / gesture while [enabled] (Android only). */
@Composable
expect fun PlatformBackHandler(enabled: Boolean, onBack: () -> Unit)

/** A webcam preview frame (desktop). */
expect fun rgbaToImageBitmap(width: Int, height: Int, rgba: ByteArray): ImageBitmap

/** A middle-button press (desktop; touch screens have none). */
expect fun PointerEvent.isMiddlePress(): Boolean
