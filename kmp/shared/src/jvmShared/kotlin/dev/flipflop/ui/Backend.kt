package dev.flipflop.ui

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.ImageBitmap
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import uniffi.flipflop.App
import uniffi.flipflop.Listener
import uniffi.flipflop.UiState
import uniffi.flipflop.isTouch
import java.io.File

/**
 * Text and/or [files] files and [folders] folders another app shared with us (Android) or dropped on the
 * window (desktop), waiting for the user to pick a group or peer; [deliver] sends them to that group once it is open.
 */
class Share(val text: String, val files: Int, val folders: Int = 0, val deliver: (group: String) -> Unit)

/** The Rust node and what it tells us, as flows the UI observes. */
class Backend(dataDir: String) {
    private val _state = MutableStateFlow<UiState?>(null)
    val state: StateFlow<UiState?> = _state

    /** One-line messages to flash. */
    val notices = MutableSharedFlow<String>(extraBufferCapacity = 16)

    /** Pages Rust asks the UI to go to. */
    val navigation = MutableSharedFlow<String>(extraBufferCapacity = 16)

    /** The latest webcam preview frame while scanning (desktop). */
    private val _camera = MutableStateFlow<ImageBitmap?>(null)
    val camera: StateFlow<ImageBitmap?> = _camera

    val touch = isTouch()

    private val themeFile = File(dataDir, "theme")

    private var _theme by mutableStateOf(runCatching { themeFile.readText().trim() }.getOrDefault("").ifEmpty { "system" })

    private val sidebarWidthFile = File(dataDir, "sidebar-width")

    private var _sidebarWidth by mutableStateOf(
        runCatching { sidebarWidthFile.readText().trim().toFloat() }.getOrNull()?.coerceIn(180f, 420f) ?: 240f,
    )

    /** Desktop sidebar width in dp, kept in the data dir once a drag ends. */
    var sidebarWidth: Float
        get() = _sidebarWidth
        set(value) {
            _sidebarWidth = value
        }

    fun saveSidebarWidth() {
        runCatching { sidebarWidthFile.writeText(_sidebarWidth.toString()) }
    }

    /** What was shared with us, until it goes somewhere (or the user cancels). */
    var share by mutableStateOf<Share?>(null)

    /** Files are being dragged over the window (desktop). */
    var dropHover by mutableStateOf(false)

    /** Files and folders dropped on the window (desktop), for the UI to send on. */
    val drops = MutableSharedFlow<List<String>>(extraBufferCapacity = 16)

    /** "system" | "light" | "dark"; kept in the data dir. */
    var theme: String
        get() = _theme
        set(value) {
            _theme = value
            runCatching { themeFile.writeText(value) }
        }

    private val listener = object : Listener {
        override fun state(state: UiState) {
            _state.value = state
        }

        override fun notice(text: String) {
            notices.tryEmit(text)
        }

        override fun navigate(page: String) {
            navigation.tryEmit(page)
        }

        override fun cameraFrame(width: UInt, height: UInt, rgba: ByteArray) {
            _camera.value = rgbaToImageBitmap(width.toInt(), height.toInt(), rgba)
        }
    }

    val app: App = App(dataDir, listener)

    init {
        if (_state.value == null) _state.value = app.state()
    }

    fun shutdown() = app.shutdown()
}

/** Start the node (and with it everything else). */
fun startBackend(dataDir: String): Backend = Backend(dataDir)

