plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

val profile = (findProperty("profile") as String?) ?: "debug"
val abis = ((findProperty("abis") as String?) ?: "arm64-v8a").split(",")

// libflipflop.so (cargo-ndk) and blew's Kotlin sources.
val buildNative = tasks.register<Exec>("buildNative") {
    workingDir = rootDir
    commandLine(listOf("./build-native.sh", profile) + abis)
    inputs.files(fileTree(rootDir.parentFile.resolve("src")), rootDir.parentFile.resolve("Cargo.toml"))
    outputs.dirs("src/main/jniLibs", "blew-kt")
}
tasks.matching { it.name == "preBuild" }.configureEach { dependsOn(buildNative) }

kotlin {
    compilerOptions { jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17) }
}

android {
    namespace = "dev.flipflop.app"
    compileSdk = 35
    ndkVersion = "27.2.12479018"

    defaultConfig {
        applicationId = "dev.flipflop.app"
        minSdk = 24
        targetSdk = 34
        val cargoVersion = Regex("""(?m)^version = "([^"]+)"""").find(rootDir.parentFile.resolve("Cargo.toml").readText())!!.groupValues[1]
        versionName = cargoVersion
        // 0.1.2 -> 1002, 1.4.7 -> 1040007
        versionCode = cargoVersion.split(".").map { it.toInt() }.let { it[0] * 1_000_000 + it[1] * 1_000 + it[2] }
        ndk { abiFilters += abis }
    }

    // Release signing: FLIPFLOP_KEYSTORE (path), FLIPFLOP_KEYSTORE_PASSWORD and optionally
    // FLIPFLOP_KEY_PASSWORD (the key alias is always "flipflop"), as environment variables or gradle properties. Without them the release APK is
    // signed with the debug key, so it installs but cannot be updated from a build signed by someone else.
    fun secret(name: String) = System.getenv(name) ?: findProperty(name) as String?
    val releaseKeystore = secret("FLIPFLOP_KEYSTORE")
    if (releaseKeystore != null) {
        signingConfigs.create("release") {
            storeFile = file(releaseKeystore)
            storePassword = secret("FLIPFLOP_KEYSTORE_PASSWORD")
            keyAlias = "flipflop"
            keyPassword = secret("FLIPFLOP_KEY_PASSWORD") ?: secret("FLIPFLOP_KEYSTORE_PASSWORD")
        }
    }
    buildTypes {
        release {
            signingConfig = signingConfigs.getByName(if (releaseKeystore != null) "release" else "debug")
            isMinifyEnabled = false
        }
    }



    sourceSets["main"].jniLibs.srcDirs("src/main/jniLibs")
    // blew's Kotlin BLE managers (copied from the crate by build-native.sh; BlewPlugin.kt is Tauri-only).
    sourceSets["main"].java.srcDirs("blew-kt")

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation(project(":shared"))
    implementation("androidx.core:core-ktx:1.15.0")
    implementation("androidx.activity:activity-compose:1.10.1")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.10.2")
    // Camera-based QR scanner activity (pure Java, no Google Play Services needed).
    implementation("com.journeyapps:zxing-android-embedded:4.3.0")
}
