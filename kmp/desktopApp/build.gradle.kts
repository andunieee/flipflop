import org.jetbrains.compose.desktop.application.dsl.TargetFormat

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

// --- Packaging ---------------------------------------------------------------------------------------
// `./gradlew :desktopApp:packageDeb | packageRpm | packageMsi | packageDmg | createDistributable`
// (each only on its own OS; see the README). The release build of the Rust library is bundled as an app
// resource and found by JNA through compose.application.resources.dir (Main.kt).

val cargoVersion = Regex("""(?m)^version = "([^"]+)"""").find(repo.resolve("Cargo.toml").readText())!!.groupValues[1]
val osName = System.getProperty("os.name").lowercase()
val osDir = when {
    osName.contains("mac") -> "macos"
    osName.contains("win") -> "windows"
    else -> "linux"
} + "-" + if (System.getProperty("os.arch").let { it == "aarch64" || it == "arm64" }) "arm64" else "x64"
val hostLib = when {
    osName.contains("mac") -> "libflipflop.dylib"
    osName.contains("win") -> "flipflop.dll"
    else -> "libflipflop.so"
}
val nativeRoot = layout.buildDirectory.dir("native")

val bundleNative = tasks.register("bundleNative") {
    val dest = nativeRoot.map { it.dir(osDir) }
    inputs.files(fileTree(repo.resolve("src")), repo.resolve("Cargo.toml"), repo.resolve("Cargo.lock"))
    outputs.dir(dest)
    doLast {
        ProcessBuilder("cargo", "build", "--release", "--lib", "--locked").directory(repo).inheritIO().start()
            .also { check(it.waitFor() == 0) { "cargo build --release failed" } }
        val lib = File(cargoTarget, "release/$hostLib")
        dest.get().asFile.apply { deleteRecursively(); mkdirs() }
        lib.copyTo(File(dest.get().asFile, hostLib))
    }
}
// Only the packaging tasks bundle the release library; `run` keeps using the debug build.
tasks.matching { it.name.startsWith("package") || it.name.startsWith("createDistributable") || it.name.startsWith("createReleaseDistributable") }
    .configureEach { dependsOn(bundleNative) }
tasks.matching { it.name == "prepareAppResources" }.configureEach { mustRunAfter(bundleNative) }

// JNA finds the debug Rust library here when running from Gradle (a packaged app bundles the release one).
tasks.matching { it.name == "run" }.configureEach {
    (this as JavaExec).jvmArgs("-Djna.library.path=$cargoTarget/debug")
}

compose.desktop {
    application {
        mainClass = "dev.flipflop.desktop.MainKt"
        nativeDistributions {
            targetFormats(TargetFormat.Deb, TargetFormat.Rpm, TargetFormat.Msi, TargetFormat.Dmg)
            packageName = "flipflop"
            packageVersion = cargoVersion
            description = "Share files, texts and a scratchpad with your devices, peer to peer."
            vendor = "andunie"
            licenseFile.set(repo.resolve("LICENSE"))
            appResourcesRootDir.set(nativeRoot)
            // The JVM modules the app needs; keeps the bundled runtime small.
            modules("java.instrument", "jdk.unsupported", "java.net.http", "java.sql")

            linux {
                iconFile.set(repo.resolve("packaging/icons/512.png"))
                packageName = "flipflop"
                debMaintainer = "andunie"
                menuGroup = "Network"
                appCategory = "Network"
                rpmLicenseType = "AGPL-3.0-or-later"
                shortcut = true
            }
            windows {
                iconFile.set(repo.resolve("packaging/windows/flipflop.ico"))
                menuGroup = "flipflop"
                shortcut = true
                perUserInstall = true
                dirChooser = true
                upgradeUuid = "8f3c6a52-5b1e-4f0c-9a57-4d2e7b1c0f31"
            }
            macOS {
                iconFile.set(repo.resolve("packaging/macos/flipflop.icns"))
                bundleID = "dev.flipflop.app"
                appCategory = "public.app-category.utilities"
                // macOS versions need a major of at least 1
                val macVersion = cargoVersion.split(".").let { "${maxOf(it[0].toInt(), 1)}.${it[1]}.${it[2]}" }
                packageVersion = macVersion
                dmgPackageVersion = macVersion
            }
        }
    }
}
