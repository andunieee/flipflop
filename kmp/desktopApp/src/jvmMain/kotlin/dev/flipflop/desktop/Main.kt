package dev.flipflop.desktop

import androidx.compose.ui.unit.DpSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application
import androidx.compose.ui.window.rememberWindowState
import dev.flipflop.ui.FlipflopApp
import dev.flipflop.ui.startBackend
import java.io.File

/** Where the node keeps its data: `FLIPFLOP_DIR`, else the platform's data directory. */
private fun dataDir(): String {
    System.getenv("FLIPFLOP_DIR")?.let { return it }
    val home = System.getProperty("user.home")
    val os = System.getProperty("os.name").lowercase()
    val base = when {
        os.contains("mac") -> "$home/Library/Application Support"
        os.contains("win") -> System.getenv("APPDATA") ?: "$home/AppData/Roaming"
        else -> System.getenv("XDG_DATA_HOME") ?: "$home/.local/share"
    }
    return File(base, "flipflop").absolutePath
}

fun main() {
    val backend = startBackend(dataDir())
    application {
        Window(
            onCloseRequest = {
                backend.shutdown()
                exitApplication()
            },
            title = "flipflop",
            state = rememberWindowState(size = DpSize(960.dp, 660.dp)),
        ) {
            window.minimumSize = java.awt.Dimension(640, 480)
            FlipflopApp(backend)
        }
    }
}
