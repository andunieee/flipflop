package dev.flipflop.desktop

import java.io.File

/**
 * `--install-desktop-entry`: puts a flipflop that no package installed (the AppImage, the portable folder) in the
 * application menu, for this user. The entry and the icon are bundled with the app (build.gradle.kts).
 */
fun installDesktopEntry() {
    check(System.getProperty("os.name").lowercase().contains("linux")) { "--install-desktop-entry only works on Linux" }
    // The AppImage itself rather than its temporary mount; otherwise the launcher jpackage started us from.
    val exe = System.getenv("APPIMAGE") ?: System.getProperty("jpackage.app-path")
        ?: error("not started from a packaged app (no launcher to point the entry at)")
    val data = File(System.getenv("XDG_DATA_HOME") ?: "${System.getProperty("user.home")}/.local/share")
    val applications = File(data, "applications")
    val icons = File(data, "icons/hicolor")

    val template = resource("dev.flipflop.app.desktop").decodeToString()
    val entry = File(applications, "dev.flipflop.app.desktop")
    applications.mkdirs()
    entry.writeText(template.replace(Regex("(?m)^Exec=flipflop$"), "Exec=${quote(exe)}"))
    for (s in listOf(16, 24, 32, 48, 64, 128, 256, 512)) {
        File(icons, "${s}x$s/apps/dev.flipflop.app.png").apply { parentFile.mkdirs() }
            .writeBytes(resource("icons/$s.png"))
    }

    // Best effort: the entry works without these, just not right away.
    runCatching { ProcessBuilder("update-desktop-database", applications.path).start().waitFor() }
    runCatching { ProcessBuilder("gtk-update-icon-cache", "-q", "-t", icons.path).start().waitFor() }
    println("Installed ${entry.path}")
}

private fun resource(name: String): ByteArray =
    object {}.javaClass.getResourceAsStream("/desktop-entry/$name")?.use { it.readBytes() } ?: error("$name is not bundled")

/** Quotes a path for the Exec key of a desktop entry. */
private fun quote(path: String): String = buildString {
    append('"')
    for (c in path) when (c) {
        '"', '`', '$', '\\' -> append("\\\\").append(c)
        '%' -> append("%%")
        else -> append(c)
    }
    append('"')
}
