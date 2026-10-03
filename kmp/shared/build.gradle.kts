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
val generateBindings = tasks.register<Exec>("generateBindings") {
    workingDir = repo
    inputs.files(fileTree(repo.resolve("src")), repo.resolve("Cargo.toml"))
    outputs.dir(bindingsDir)
    doFirst { bindingsDir.get().asFile.deleteRecursively() }
    commandLine(
        "bash", "-c",
        "cargo build --lib && cargo run -q --features bindgen --bin uniffi-bindgen -- generate " +
            "--library '$cargoTarget/debug/$hostLib' --language kotlin --no-format --out-dir '${bindingsDir.get().asFile}'",
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
            implementation(compose.runtime)
            implementation(compose.foundation)
            implementation(compose.material3)
            implementation(compose.ui)
            implementation(compose.components.resources)
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
