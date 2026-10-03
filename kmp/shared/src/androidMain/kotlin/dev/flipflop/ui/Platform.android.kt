package dev.flipflop.ui

import android.graphics.Bitmap
import androidx.activity.compose.BackHandler
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap

@Composable
actual fun PlatformBackHandler(enabled: Boolean, onBack: () -> Unit) = BackHandler(enabled, onBack)

actual fun rgbaToImageBitmap(width: Int, height: Int, rgba: ByteArray): ImageBitmap {
    val pixels = IntArray(width * height) { i ->
        val o = i * 4
        (rgba[o + 3].toInt() and 0xff shl 24) or (rgba[o].toInt() and 0xff shl 16) or
            (rgba[o + 1].toInt() and 0xff shl 8) or (rgba[o + 2].toInt() and 0xff)
    }
    return Bitmap.createBitmap(pixels, width, height, Bitmap.Config.ARGB_8888).asImageBitmap()
}
