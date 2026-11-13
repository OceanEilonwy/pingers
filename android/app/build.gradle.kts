plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

// The Rust core (crates/pingers-ffi, UniFFI) is not wired in yet: the app runs on
// FakeRepository. When it is, the cargo tasks from monokulo-fob's app build
// (cargoHost, uniffiBindings, cargoNdk) go here and add their outputs as generated
// Kotlin sources and jniLibs. See android/README.md.

android {
    namespace = "org.pingers.app"
    compileSdk = 37
    compileSdkMinor = 2

    defaultConfig {
        applicationId = "org.pingers.app"
        minSdk = 29
        targetSdk = 37
        versionCode = 1
        versionName = "0.1.0"
    }

    buildTypes {
        debug {
            applicationIdSuffix = ".debug"
        }
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"))
        }
    }

    buildFeatures {
        compose = true
    }
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(libs.lifecycle.runtime.compose)
    implementation(libs.kotlinx.coroutines.android)
    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.material3)
    implementation(libs.compose.ui.tooling.preview)
    debugImplementation(libs.compose.ui.tooling)
}
