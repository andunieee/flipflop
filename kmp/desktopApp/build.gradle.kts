plugins {
    id("org.jetbrains.kotlin.multiplatform")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jetbrains.compose")
}

kotlin {
    jvm {
        compilerOptions.jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
    sourceSets {
        jvmMain.dependencies {
            implementation(project(":shared"))
            implementation(compose.desktop.currentOs)
        }
    }
}

val repo = rootDir.parentFile
val cargoTarget: String by lazy {
    val json = ProcessBuilder("cargo", "metadata", "--format-version", "1", "--no-deps").directory(repo).start()
        .inputStream.bufferedReader().readText()
    Regex("\"target_directory\":\"([^\"]+)\"").find(json)!!.groupValues[1]
}

compose.desktop {
    application {
        mainClass = "dev.flipflop.desktop.MainKt"
        // JNA finds the Rust library here (a release build is bundled later).
        jvmArgs += listOf("-Djna.library.path=$cargoTarget/debug")
    }
}
