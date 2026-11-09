import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("org.jetbrains.kotlin.multiplatform")
    id("com.android.library")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jetbrains.compose")
}

val repo = rootDir.parentFile

// The Rust crate's target dir (CARGO_TARGET_DIR may point elsewhere).
val cargoTarget: String by lazy {
    val json = ProcessBuilder("cargo", "metadata", "--format-version", "1", "--no-deps").directory(repo).start()
        .inputStream.bufferedReader().readText()
    Regex("\"target_directory\":\"([^\"]+)\"").find(json)!!.groupValues[1]
}
val hostLib = when {
    System.getProperty("os.name").startsWith("Mac") -> "libflipflop.dylib"
    System.getProperty("os.name").startsWith("Windows") -> "flipflop.dll"
    else -> "libflipflop.so"
}
val bindingsDir = layout.buildDirectory.dir("generated/uniffi")

// The Kotlin side of the UniFFI bridge (src/ffi.rs), generated from the built library.
// Invoke cargo directly rather than through `bash -c`: on Windows `bash` is WSL, which is not installed.
val buildBindingsLibrary = tasks.register<Exec>("buildBindingsLibrary") {
    workingDir = repo
    commandLine("cargo", "build", "--lib")
    inputs.files(fileTree(repo.resolve("src")), repo.resolve("Cargo.toml"), repo.resolve("Cargo.lock"))
    outputs.file(File(cargoTarget, "debug/$hostLib"))
}
val generateBindings = tasks.register<Exec>("generateBindings") {
    dependsOn(buildBindingsLibrary)
    workingDir = repo
    inputs.files(fileTree(repo.resolve("src")), repo.resolve("Cargo.toml"), repo.resolve("Cargo.lock"))
    outputs.dir(bindingsDir)
    doFirst { bindingsDir.get().asFile.deleteRecursively() }
    commandLine(
        "cargo", "run", "-q", "--features", "bindgen", "--bin", "uniffi-bindgen", "--", "generate",
        "--library", File(cargoTarget, "debug/$hostLib").absolutePath,
        "--language", "kotlin", "--no-format",
        "--out-dir", bindingsDir.get().asFile.absolutePath,
    )
}

kotlin {
    androidTarget {
        compilerOptions.jvmTarget.set(JvmTarget.JVM_17)
    }
    jvm("desktop") {
        compilerOptions.jvmTarget.set(JvmTarget.JVM_17)
    }

    sourceSets {
        // The generated bindings talk to the native library through JNA, so everything that uses them is
        // shared between the two JVM targets only.
        val jvmShared by creating {
            dependsOn(commonMain.get())
            kotlin.srcDir(generateBindings)
        }
        androidMain.get().dependsOn(jvmShared)
        val desktopMain by getting { dependsOn(jvmShared) }

        commonMain.dependencies {
            // The compose.* accessors are deprecated in Compose Multiplatform 1.10; spell the
            // coordinates out. Keep these versions in step with the plugin in kmp/build.gradle.kts
            // (material3 has its own release line).
            implementation("org.jetbrains.compose.runtime:runtime:1.10.3")
            implementation("org.jetbrains.compose.foundation:foundation:1.10.3")
            implementation("org.jetbrains.compose.material3:material3:1.9.0")
            implementation("org.jetbrains.compose.ui:ui:1.10.3")
            implementation("org.jetbrains.compose.components:components-resources:1.10.3")
        }
        androidMain.dependencies {
            implementation("net.java.dev.jna:jna:5.17.0@aar")
            implementation("androidx.activity:activity-compose:1.10.1")
        }
        desktopMain.dependencies {
            implementation("net.java.dev.jna:jna:5.17.0")
            implementation(compose.desktop.currentOs)
        }
    }
}

android {
    namespace = "dev.flipflop.shared"
    compileSdk = 35
    defaultConfig { minSdk = 24 }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

compose.resources {
    packageOfResClass = "dev.flipflop.ui.res"
    publicResClass = false
}
