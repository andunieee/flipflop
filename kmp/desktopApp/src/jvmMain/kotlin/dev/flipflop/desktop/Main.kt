package dev.flipflop.desktop

import androidx.compose.foundation.draganddrop.dragAndDropTarget
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.remember
import androidx.compose.ui.ExperimentalComposeUiApi
import androidx.compose.ui.Modifier
import androidx.compose.ui.draganddrop.DragAndDropEvent
import androidx.compose.ui.draganddrop.DragAndDropTarget
import androidx.compose.ui.draganddrop.DragData
import androidx.compose.ui.draganddrop.dragData
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.DpSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application
import androidx.compose.ui.window.rememberWindowState
import dev.flipflop.ui.Backend
import dev.flipflop.ui.FlipflopApp
import dev.flipflop.ui.startBackend
import java.io.File
import java.net.URI

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

/** Files and folders dropped on the window go to the group showing, else to a group or peer the user picks, like a share on Android. */
@OptIn(ExperimentalComposeUiApi::class)
private fun dropTarget(backend: Backend) = object : DragAndDropTarget {
    override fun onEntered(event: DragAndDropEvent) {
        backend.dropHover = true
    }

    override fun onExited(event: DragAndDropEvent) {
        backend.dropHover = false
    }

    override fun onEnded(event: DragAndDropEvent) {
        backend.dropHover = false
    }

    override fun onDrop(event: DragAndDropEvent): Boolean {
        backend.dropHover = false
        val files = event.dragData() as? DragData.FilesList ?: return false
        val paths = files.readFiles().mapNotNull { if (it.startsWith("file:")) runCatching { File(URI(it)).path }.getOrNull() else it }
        if (paths.isNotEmpty()) backend.drops.tryEmit(paths)
        return paths.isNotEmpty()
    }
}

@OptIn(ExperimentalComposeUiApi::class)
fun main(args: Array<String>) {
    if ("--install-desktop-entry" in args) {
        try {
            installDesktopEntry()
        } catch (e: Exception) {
            System.err.println("Could not install the desktop entry: ${e.message}")
            kotlin.system.exitProcess(1)
        }
        return
    }
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
            Box(Modifier.fillMaxSize().dragAndDropTarget({ it.dragData() is DragData.FilesList }, remember(backend) { dropTarget(backend) })) {
                FlipflopApp(backend)
            }
        }
    }
}
