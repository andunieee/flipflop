package dev.flipflop.app

import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import dev.flipflop.ui.Backend
import dev.flipflop.ui.FlipflopApp
import dev.flipflop.ui.Share
import dev.flipflop.ui.startBackend

class MainActivity : FlipflopActivity() {
    private companion object {
        /** The node: one per process, which can outlive an activity while files are moving. */
        var node: Backend? = null
    }

    private lateinit var backend: Backend

    override fun onCreate(savedInstanceState: Bundle?) {
        // FlipflopActivity hands the activity to Rust first; the node starts after that.
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        backend = node ?: startBackend(filesDir.absolutePath).also { node = it }
        // Reopened from Recents, Android hands us the share we already took care of.
        if (savedInstanceState == null && (intent.flags and Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY) == 0) takeShare(intent)
        setContent { FlipflopApp(backend) }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        takeShare(intent)
    }

    /** Text or files shared from another app: the UI asks where they go. */
    private fun takeShare(intent: Intent) {
        if (intent.action != Intent.ACTION_SEND && intent.action != Intent.ACTION_SEND_MULTIPLE) return
        val text = intent.getCharSequenceExtra(Intent.EXTRA_TEXT)?.toString()?.trim().orEmpty()
        val uris = streams(intent)
        if (text.isEmpty() && uris.isEmpty()) return
        backend.share = Share(text, uris.size) { group ->
            if (text.isNotEmpty()) backend.app.sendText(text)
            if (uris.isNotEmpty()) stageShared(uris, group)
        }
    }

    @Suppress("DEPRECATION")
    private fun streams(intent: Intent): List<Uri> {
        val uris = if (intent.action == Intent.ACTION_SEND_MULTIPLE) {
            if (Build.VERSION.SDK_INT >= 33) intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri::class.java)
            else intent.getParcelableArrayListExtra<Uri>(Intent.EXTRA_STREAM)
        } else {
            listOfNotNull(
                if (Build.VERSION.SDK_INT >= 33) intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)
                else intent.getParcelableExtra<Uri>(Intent.EXTRA_STREAM)
            )
        }
        return uris.orEmpty().filterNotNull()
    }
}
