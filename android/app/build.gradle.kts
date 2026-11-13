plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

// The Rust core is crates/pingers-ffi, through UniFFI. Both the native libraries and the
// Kotlin bindings are built here from the crates, so they can't drift: building needs Rust,
// cargo-ndk and the NDK below. Debug builds use cargo's dev profile; release builds use the
// workspace's `android` profile (LTO, one codegen unit, stripped).
val repo: File = rootDir.parentFile
// The host library is only read by uniffi-bindgen, for the bindings; the dev build is enough.
val hostLibrary = File(repo, "target/debug/" + System.mapLibraryName("pingers_ffi"))
val hostBindgen = File(repo, "target/debug/uniffi-bindgen")
// Phones, and the emulator.
val abis = listOf("arm64-v8a", "x86_64")
val minSdkVersion = 29
// -PpingersSplits builds an APK for each ABI plus a universal one; any other build is one APK
// holding both.
val splitting = project.hasProperty("pingersSplits")
// -PpingersFcm adds the FCM push service (src/fcm). It needs google-services credentials, which
// the project doesn't have yet; without the flag push is UnifiedPush only.
val fcm = project.hasProperty("pingersFcm")

/** A cargo command that writes one directory, so AGP can take it as generated sources. */
abstract class CargoTask : Exec() {
    @get:OutputDirectory
    abstract val outDir: DirectoryProperty
}

android {
    namespace = "org.pingers.app"
    compileSdk = 37
    compileSdkMinor = 2
    ndkVersion = "27.0.12077973"

    defaultConfig {
        applicationId = "org.pingers.app"
        minSdk = minSdkVersion
        targetSdk = 37
        versionCode = 1
        versionName = "0.1.0"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    testOptions {
        unitTests.isIncludeAndroidResources = true
    }

    // Only the ABIs pingers-ffi is built for: JNA's AAR also carries 32-bit libraries, and a
    // 32-bit phone that installed them would crash at the first call into Rust. rln also
    // declares itself a cdylib, so cargo-ndk copies a librln-*.so beside ours; pingers-ffi links
    // rln statically and never loads it.
    packaging {
        jniLibs {
            excludes += listOf("lib/armeabi/**", "lib/armeabi-v7a/**", "lib/x86/**", "lib/mips/**", "lib/mips64/**")
            excludes += "lib/*/librln-*.so"
        }
    }

    splits {
        abi {
            isEnable = splitting
            reset()
            include(*abis.toTypedArray())
            isUniversalApk = true
        }
    }

    buildTypes {
        debug {
            applicationIdSuffix = ".debug"
        }
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
    }

    buildFeatures {
        compose = true
    }
}

val cargoHost = tasks.register<Exec>("cargoHost") {
    description = "Builds pingers-ffi and uniffi-bindgen for this machine, to generate the bindings from."
    workingDir = repo
    commandLine("cargo", "build", "--locked", "-p", "pingers-ffi", "--features", "bindgen")
    inputs.dir(File(repo, "crates"))
    inputs.files(File(repo, "Cargo.lock"), File(repo, "Cargo.toml"))
    outputs.files(hostLibrary, hostBindgen)
}

val uniffiBindings = tasks.register<CargoTask>("uniffiBindings") {
    description = "Generates pingers-ffi's Kotlin bindings."
    dependsOn(cargoHost)
    workingDir = File(repo, "crates/pingers-ffi")
    outDir.set(layout.buildDirectory.dir("rust/kotlin"))
    inputs.files(hostLibrary, hostBindgen, File(repo, "crates/pingers-ffi/uniffi.toml"))
    argumentProviders.add(CommandLineArgumentProvider { listOf(outDir.get().asFile.absolutePath) })
    commandLine(
        hostBindgen.absolutePath, "generate", "--library", hostLibrary.absolutePath,
        "--language", "kotlin", "--no-format", "--out-dir",
    )
}

/** Builds pingers-ffi for each Android ABI with [profile] into build/rust/jniLibs-[name]. */
fun cargoNdk(name: String, profile: String) = tasks.register<CargoTask>("cargoNdk${name.replaceFirstChar { it.uppercase() }}") {
    description = "Builds pingers-ffi for each Android ABI ($profile profile)."
    workingDir = repo
    outDir.set(layout.buildDirectory.dir("rust/jniLibs-$name"))
    inputs.dir(File(repo, "crates"))
    inputs.files(File(repo, "Cargo.lock"), File(repo, "Cargo.toml"))
    // This module's NDK, whatever the environment says.
    val ndk = androidComponents.sdkComponents.ndkDirectory.map { it.asFile.absolutePath }
    doFirst {
        environment("ANDROID_NDK_HOME", ndk.get())
        environment("NDK_HOME", ndk.get())
    }
    commandLine(listOf("cargo", "ndk") + abis.flatMap { listOf("-t", it) } + listOf("--platform", "$minSdkVersion", "-o"))
    argumentProviders.add(
        CommandLineArgumentProvider {
            listOf(outDir.get().asFile.absolutePath, "build", "--profile", profile, "--locked", "-p", "pingers-ffi")
        },
    )
}

val cargoNdkDebug = cargoNdk("debug", "dev")
val cargoNdkRelease = cargoNdk("release", "android")

androidComponents {
    onVariants { variant ->
        val native = if (variant.buildType == "release") cargoNdkRelease else cargoNdkDebug
        variant.sources.jniLibs?.addGeneratedSourceDirectory(native, CargoTask::outDir)
        variant.sources.kotlin?.addGeneratedSourceDirectory(uniffiBindings, CargoTask::outDir)
        if (fcm) {
            variant.sources.kotlin?.addStaticSourceDirectory("src/fcm/kotlin")
            variant.sources.manifests.addStaticManifestFile("src/fcm/AndroidManifest.xml")
        }
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
    // JNA as an AAR for the app: it carries JNA's own native library. UniFFI's bindings use it.
    implementation("${libs.jna.get()}@aar")
    debugImplementation(libs.compose.ui.tooling)

    // Platform layer.
    implementation(libs.play.services.location)
    implementation(libs.unifiedpush.connector)
    implementation(libs.work.runtime)
    if (fcm) implementation(libs.firebase.messaging)

    testImplementation(libs.junit)
    testImplementation(libs.robolectric)
    testImplementation(libs.androidx.test.core)
    testImplementation(libs.work.testing)
    androidTestImplementation(libs.androidx.test.ext.junit)
    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.androidx.test.core)
}
