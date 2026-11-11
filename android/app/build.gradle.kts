import java.util.Properties

plugins {
    id("com.android.application")
    // blew's BLE layer is Kotlin (copied from the crate by build-native.sh); the app itself is Java.
    id("org.jetbrains.kotlin.android")
}

val profile = (findProperty("profile") as String?) ?: "debug"
val abis = ((findProperty("abis") as String?) ?: "arm64-v8a").split(",")

// libflipflop_app.so (the Freya UI and the node, built by cargo-ndk) and blew's Kotlin sources.
val buildNative = tasks.register<Exec>("buildNative") {
    workingDir = rootDir
    commandLine(listOf("./build-native.sh", profile) + abis)
    val repo = rootDir.parentFile
    inputs.files(
        fileTree(repo.resolve("src")), fileTree(repo.resolve("app/src")), repo.resolve("app/icons"),
        repo.resolve("Cargo.toml"), repo.resolve("app/Cargo.toml"), repo.resolve("Cargo.lock"),
    )
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

    // Release signing, in order of precedence:
    //   1. ANDROID_KEYSTORE (path), ANDROID_KEYSTORE_PASSWORD, ANDROID_KEY_ALIAS and ANDROID_KEY_PASSWORD, as
    //      environment variables or gradle properties. CI sets these from the ANDROID_KEYSTORE_* secrets (release.yml).
    //   2. The signing properties file (storeFile, storePassword, keyAlias, keyPassword): android/keystore.properties
    //      if it exists, else the shared ~/android-release/signing.properties. Set ANDROID_SIGNING_PROPERTIES to use
    //      another path. This is what makes local release builds use the stable key without exporting anything.
    //   3. The older FLIPFLOP_KEYSTORE* names, for setups that predate the shared key.
    // Without any of them the release APK is signed with the debug key, so it installs but cannot be updated from a
    // build signed by someone else. CI exports unset secrets as empty strings, so treat blank as missing: otherwise a
    // missing ANDROID_KEY_PASSWORD would become "" and fail with "Given final block not properly padded".
    fun secret(vararg names: String) = names.firstNotNullOfOrNull { name ->
        (System.getenv(name) ?: findProperty(name) as String?)?.takeIf { it.isNotBlank() }
    }
    val signingProperties = secret("ANDROID_SIGNING_PROPERTIES")?.let { file(it) }
        ?: listOf(
            rootProject.file("keystore.properties"),
            file(System.getProperty("user.home")).resolve("android-release/signing.properties"),
        ).firstOrNull { it.isFile }
    val shared = signingProperties?.takeIf { it.isFile }?.let { propertiesFile ->
        Properties().apply { propertiesFile.inputStream().use { load(it) } }
    }
    val releaseKeystore = secret("ANDROID_KEYSTORE", "FLIPFLOP_KEYSTORE") ?: shared?.getProperty("storeFile")
    if (releaseKeystore != null) {
        val storePassword = secret("ANDROID_KEYSTORE_PASSWORD", "FLIPFLOP_KEYSTORE_PASSWORD")
            ?: shared?.getProperty("storePassword")
            ?: error("$releaseKeystore is set but neither ANDROID_KEYSTORE_PASSWORD nor the signing properties file has the password")
        signingConfigs.create("release") {
            storeFile = file(releaseKeystore)
            this.storePassword = storePassword
            keyAlias = secret("ANDROID_KEY_ALIAS", "FLIPFLOP_KEY_ALIAS")
                ?: shared?.getProperty("keyAlias") ?: "flipflop"
            // PKCS12 keystores use one password for the store and the key; JKS may have a separate one.
            keyPassword = secret("ANDROID_KEY_PASSWORD", "FLIPFLOP_KEY_PASSWORD")
                ?: shared?.getProperty("keyPassword") ?: storePassword
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
    // TransferService's notification, and the window insets.
    implementation("androidx.core:core:1.15.0")
    // Camera-based QR scanner activity (pure Java, no Google Play Services needed).
    implementation("com.journeyapps:zxing-android-embedded:4.3.0")
}
