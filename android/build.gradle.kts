plugins {
    id("com.android.application") version "8.13.2" apply false
    // Only for blew's BLE layer, which ships as Kotlin sources inside the crate (see build-native.sh).
    id("org.jetbrains.kotlin.android") version "2.3.20" apply false
}
