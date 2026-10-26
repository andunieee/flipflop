package dev.flipflop.app

import android.os.Bundle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import dev.flipflop.ui.FlipflopApp
import dev.flipflop.ui.startBackend

class MainActivity : FlipflopActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        // FlipflopActivity hands the activity to Rust first; the node starts after that.
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val backend = startBackend(filesDir.absolutePath)
        setContent { FlipflopApp(backend) }
    }
}
