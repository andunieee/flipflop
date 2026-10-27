package dev.flipflop.desktop

import androidx.compose.ui.res.painterResource
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

/** A packaged app carries libflipflop in its resources; a Gradle run sets `jna.library.path` itself. */
private fun pointJnaAtBundledLibrary() {
    if (System.getProperty("jna.library.path") != null) return
    System.getProperty("compose.application.resources.dir")?.let { System.setProperty("jna.library.path", it) }
}

fun main() {
    pointJnaAtBundledLibrary()
    val backend = startBackend(dataDir())
    application {
        Window(
            onCloseRequest = {
                backend.shutdown()
                exitApplication()
            },
            title = "flipflop",
            icon = painterResource("icon.png"),
            state = rememberWindowState(size = DpSize(960.dp, 660.dp)),
        ) {
            window.minimumSize = java.awt.Dimension(640, 480)
            FlipflopApp(backend)
        }
    }
}
