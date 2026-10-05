package dev.flipflop.ui

import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.toComposeImageBitmap
import androidx.compose.ui.input.pointer.PointerButton
import androidx.compose.ui.input.pointer.PointerEvent
import androidx.compose.ui.input.pointer.PointerEventType
import org.jetbrains.skia.ColorAlphaType
import org.jetbrains.skia.ColorType
import org.jetbrains.skia.Image
import org.jetbrains.skia.ImageInfo

@Composable
actual fun PlatformBackHandler(enabled: Boolean, onBack: () -> Unit) {}

actual fun rgbaToImageBitmap(width: Int, height: Int, rgba: ByteArray): ImageBitmap =
    Image.makeRaster(ImageInfo(width, height, ColorType.RGBA_8888, ColorAlphaType.UNPREMUL), rgba, width * 4)
        .toComposeImageBitmap()

@OptIn(androidx.compose.ui.ExperimentalComposeUiApi::class)
actual fun PointerEvent.isMiddlePress(): Boolean = type == PointerEventType.Press && button == PointerButton.Tertiary
