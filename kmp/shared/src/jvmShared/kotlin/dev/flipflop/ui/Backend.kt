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

