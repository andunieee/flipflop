package dev.flipflop.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.runtime.remember
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** App-wide palette: sky blue, white cards, deep navy text. Light and dark values. */
class Palette(private val dark: Boolean) {
    private fun c(light: Long, darkv: Long) = Color(if (dark) darkv else light)

    val windowBg = c(0xFFf4faff, 0xFF0e1824)
    val sidebarBg = c(0xFFe6f3fd, 0xFF0a131c)
    val cardBg = c(0xFFffffff, 0xFF15222f)
    val insetBg = c(0xFFf2f8fd, 0xFF111c28)
    val border = c(0xFFd6e7f5, 0xFF223446)
    val borderStrong = c(0xFFb8d4ec, 0xFF2e4660)
    val shadow = c(0x401e5a8a, 0x80000000)

    val textStrong = c(0xFF10304a, 0xFFe8f2fb)
    val text = c(0xFF2c4a63, 0xFFc2d3e2)
    val textMuted = c(0xFF66819a, 0xFF8aa1b6)
    val textFaint = c(0xFF95acc0, 0xFF5d7489)

    val accent = Color(0xFF1e8cf0)
    val accentHover = Color(0xFF3a9ff5)
    val accentPress = Color(0xFF1774cc)
    val accentSoft = c(0xFFdbeeff, 0xFF12324f)
    val accentText = c(0xFF1366b8, 0xFF6db8ff)

    val online = c(0xFF22b55a, 0xFF2fcb68)
    val offline = c(0xFFc2d2df, 0xFF3a4d60)
    val warn = c(0xFFe08a00, 0xFFf0a32a)
    val warnSoft = c(0xFFfff1d6, 0xFF3a2b0e)
    val info = c(0xFF0b8fd6, 0xFF3fb2f0)
    val infoSoft = c(0xFFdcf2fd, 0xFF0f3047)
    val error = c(0xFFe5484d, 0xFFf2676b)
    val errorSoft = c(0xFFffe5e5, 0xFF3b1a1e)
    val errorSoftPress = c(0xFFffd0d1, 0xFF4d2026)
    val success = c(0xFF179c4a, 0xFF3acb70)
    val successSoft = c(0xFFdcf6e5, 0xFF123423)

    val chipBg = c(0xFFe8f2fb, 0xFF1c2b3b)
    val chipBgHover = c(0xFFd9e9f7, 0xFF243649)
    val chipBgPress = c(0xFFc9dff2, 0xFF2c4157)
}

/** Sizes; the phone layout is a little larger. */
class Dim(touch: Boolean, val narrow: Boolean) {
    val radiusSm: Dp = 6.dp
    val radius: Dp = 10.dp
    val radiusLg: Dp = 14.dp

    val space1: Dp = 4.dp
    val space2: Dp = 8.dp
    val space3: Dp = 12.dp
    val space4: Dp = 16.dp
    val space5: Dp = 24.dp

    val fontXs = if (touch) 12.sp else 11.sp
    val fontSm = if (touch) 13.sp else 12.sp
    val fontMd = if (touch) 15.sp else 13.sp
    val fontLg = if (touch) 17.sp else 15.sp
    val fontXl = if (touch) 20.sp else 22.sp

    val controlHeight: Dp = if (touch) 44.dp else 32.dp
    val pagePadding: Dp = if (touch) 16.dp else 28.dp
    val cardPadding: Dp = if (touch) 14.dp else 16.dp

    /** Side padding of a labelled button; tighter on narrow windows. */
    val buttonPadding: Dp = if (narrow) 8.dp else 14.dp
}

val LocalPalette = compositionLocalOf { Palette(false) }
val LocalDim = compositionLocalOf { Dim(false, false) }

object Theme {
    val c: Palette @Composable get() = LocalPalette.current
    val d: Dim @Composable get() = LocalDim.current
}

@Composable
fun FlipflopTheme(touch: Boolean, narrow: Boolean, content: @Composable () -> Unit) {
    val dark = isSystemInDarkTheme()
    val palette = remember(dark) { Palette(dark) }
    val dim = remember(touch, narrow) { Dim(touch, narrow) }
    CompositionLocalProvider(LocalPalette provides palette, LocalDim provides dim) {
        MaterialTheme(colorScheme = lightColorScheme(primary = palette.accent, background = palette.windowBg), content = content)
    }
}
