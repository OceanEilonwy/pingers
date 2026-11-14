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
// Firebase's google-services.json for the gms flavour's FCM. It is never in the repository:
// point at it with the Gradle property pingers.googleServicesJson (for example in
// ~/.gradle/gradle.properties) or PINGERS_GOOGLE_SERVICES_JSON. Without it the gms flavour
// still builds, with FCM off (push is UnifiedPush only).
val googleServicesJson: Provider<RegularFile> = providers.gradleProperty("pingers.googleServicesJson")
    .orElse(providers.environmentVariable("PINGERS_GOOGLE_SERVICES_JSON"))
    .map { layout.projectDirectory.file(File(it).absolutePath) }
val firebasePackage = "org.pingers.app"

// A test CA for a local stack (PINGERS_EXTRA_CA, or -PpingersExtraCa): a PEM file's path
// (relative to the repository, as `cargo xtask dev-ca` writes target/dev-ca/ca.pem) or the PEM
// itself. Debug builds only: the core then trusts that CA instead of Mozilla's roots. Release
// builds never read it (see buildTypes), so a CI variable left set cannot reach a release.
val extraCaPem: String = providers.gradleProperty("pingersExtraCa")
    .orElse(providers.environmentVariable("PINGERS_EXTRA_CA"))
    .map { value ->
        if (value.trimStart().startsWith("-----BEGIN")) {
            value
        } else {
            val file = File(value).let { if (it.isAbsolute) it else File(repo, value) }
            if (!file.isFile) throw GradleException("PINGERS_EXTRA_CA does not name a file: $value")
            file.readText()
        }
    }
    .getOrElse("")

// The first owner's registration code for a server that requires one
// (PINGERS_DEV_REGISTRATION_CODE, or -PpingersDevRegistrationCode): the code nodo generated for
// the server (docs/deploy-nodo.md).
// Debug builds only, like the test CA: release builds never read it (see buildTypes). Other
// devices get the code from the invite link they open.
val devRegistrationCode: String = providers.gradleProperty("pingersDevRegistrationCode")
    .orElse(providers.environmentVariable("PINGERS_DEV_REGISTRATION_CODE"))
    .getOrElse("")
    .trim()

/** A Kotlin string literal for BuildConfig. */
fun literal(text: String): String =
    "\"" + text.replace("\\", "\\\\").replace("\"", "\\\"").replace("\r", "").replace("\n", "\\n") + "\""

// Release (upload) signing, from the environment only: never a file in the repository, never a
// Gradle property that could sit in a checked-in gradle.properties. PINGERS_KEYSTORE_PATH is a
// PKCS12 keystore (CI decodes PINGERS_KEYSTORE_BASE64 into a temporary file and points this at
// it), PINGERS_KEYSTORE_PASSWORD unlocks both the store and the key (PKCS12 has one password),
// and PINGERS_KEY_ALIAS names the key. Without all three a release build stops at
// checkReleaseSigning; it is never signed with the debug key. See docs/release.md.
val releaseKeystorePath: String? = providers.environmentVariable("PINGERS_KEYSTORE_PATH").orNull
val releaseKeystorePassword: String? = providers.environmentVariable("PINGERS_KEYSTORE_PASSWORD").orNull
val releaseKeyAlias: String? = providers.environmentVariable("PINGERS_KEY_ALIAS").orNull
val signingProblems: List<String> = buildList {
    if (releaseKeystorePath.isNullOrBlank()) {
        add(
            if (providers.environmentVariable("PINGERS_KEYSTORE_BASE64").isPresent) {
                "PINGERS_KEYSTORE_PATH is unset (PINGERS_KEYSTORE_BASE64 is set: decode it into a " +
                    "temporary file outside the checkout and point PINGERS_KEYSTORE_PATH at it)"
            } else {
                "PINGERS_KEYSTORE_PATH is unset"
            },
        )
    } else if (!File(releaseKeystorePath).isFile) {
        add("PINGERS_KEYSTORE_PATH does not name a file: $releaseKeystorePath")
    }
    if (releaseKeystorePassword.isNullOrEmpty()) add("PINGERS_KEYSTORE_PASSWORD is unset")
    if (releaseKeyAlias.isNullOrBlank()) add("PINGERS_KEY_ALIAS is unset")
}

/** Fails a release build that has no signing key, before anything is compiled. */
abstract class CheckReleaseSigning : DefaultTask() {
    @get:Input
    abstract val problems: ListProperty<String>

    @TaskAction
    fun check() {
        val problems = problems.get()
        if (problems.isNotEmpty()) {
            throw GradleException(
                "Release builds must be signed with the Pingers upload key, and it is not configured:\n" +
                    problems.joinToString("\n") { "  - $it" } +
                    "\nSet PINGERS_KEYSTORE_PATH, PINGERS_KEYSTORE_PASSWORD and PINGERS_KEY_ALIAS, or run " +
                    "`cargo xtask release-android`. See android/docs/release.md.",
            )
        }
    }
}

val checkReleaseSigning = tasks.register<CheckReleaseSigning>("checkReleaseSigning") {
    description = "Fails unless the release signing key is configured in the environment."
    problems.set(signingProblems)
    // Never up to date: it must run, and fail, on every release build without a key.
    outputs.upToDateWhen { false }
}

/** A cargo command that writes one directory, so AGP can take it as generated sources. */
abstract class CargoTask : Exec() {
    @get:OutputDirectory
    abstract val outDir: DirectoryProperty
}

/**
 * Copies google-services.json into the build directory and writes the Android resources the
 * Google Services plugin would (FirebaseApp reads them at start-up), without that plugin, which
 * wants the file inside the source tree.
 */
abstract class GoogleServicesTask : DefaultTask() {
    @get:InputFile
    abstract val json: RegularFileProperty

    @get:Input
    abstract val packageName: Property<String>

    @get:OutputDirectory
    abstract val resDir: DirectoryProperty

    @get:OutputFile
    abstract val copy: RegularFileProperty

    @TaskAction
    fun generate() {
        val source = json.get().asFile
        source.copyTo(copy.get().asFile, overwrite = true)
        @Suppress("UNCHECKED_CAST")
        val root = groovy.json.JsonSlurper().parse(source) as Map<String, Any?>
        val project = root["project_info"] as Map<String, Any?>
        val clients = root["client"] as List<Map<String, Any?>>
        val client = clients.firstOrNull {
            val info = it["client_info"] as Map<String, Any?>
            (info["android_client_info"] as Map<String, Any?>)["package_name"] == packageName.get()
        } ?: throw GradleException("google-services.json has no client for ${packageName.get()}")
        val appId = (client["client_info"] as Map<String, Any?>)["mobilesdk_app_id"] as String
        val apiKey = ((client["api_key"] as List<Map<String, Any?>>).first())["current_key"] as String
        val values = linkedMapOf(
            "google_app_id" to appId,
            "gcm_defaultSenderId" to project["project_number"].toString(),
            "google_api_key" to apiKey,
            "google_crash_reporting_api_key" to apiKey,
            "project_id" to project["project_id"].toString(),
        )
        (project["storage_bucket"] as String?)?.let { values["google_storage_bucket"] = it }
        val out = resDir.get().asFile.resolve("values")
        out.mkdirs()
        out.resolve("google-services.xml").writeText(
            buildString {
                appendLine("<?xml version=\"1.0\" encoding=\"utf-8\"?>")
                appendLine("<!-- Generated from google-services.json at build time. -->")
                appendLine("<resources>")
                values.forEach { (k, v) -> appendLine("    <string name=\"$k\" translatable=\"false\">$v</string>") }
                appendLine("</resources>")
            },
        )
    }
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
        // The publisher's map host, the last link of the map host chain (a user override, then
        // the host the Pingers server advertises, then this). Deployment configuration, never
        // source: CI sets PINGERS_MAP_HOST, or pass `-PpingersMapHost=https://maps.example.org`.
        // Unset, it is a host that never resolves, so a local build contacts nobody's server.
        val mapHost = providers.gradleProperty("pingersMapHost")
            .orElse(providers.environmentVariable("PINGERS_MAP_HOST"))
            .getOrElse("https://maps.pingers.invalid")
        buildConfigField("String", "MAP_HOST", "\"$mapHost\"")
        // The Pingers instance the core talks to (Core.open): the server and OHTTP gateway, the
        // OHTTP relay and the push relay. Deployment configuration as above: CI sets
        // PINGERS_SERVER_HOST, PINGERS_OHTTP_RELAY and PINGERS_PUSH_RELAY (or pass
        // -PpingersServerHost=..., -PpingersOhttpRelay=..., -PpingersPushRelay=...). Unset, they
        // are hosts that never resolve.
        fun deployment(property: String, variable: String, unset: String) {
            val value = providers.gradleProperty(property)
                .orElse(providers.environmentVariable(variable))
                .getOrElse(unset)
            buildConfigField("String", variable.removePrefix("PINGERS_"), "\"$value\"")
        }
        deployment("pingersServerHost", "PINGERS_SERVER_HOST", "https://pingers.invalid")
        deployment("pingersOhttpRelay", "PINGERS_OHTTP_RELAY", "https://ohttp.pingers.invalid")
        deployment("pingersPushRelay", "PINGERS_PUSH_RELAY", "https://push.pingers.invalid")
        // The key config hashes the gateway and the push relay may have: for each, the key
        // served now and the next one (two-slot rotation), base64url SHA-256, comma-separated,
        // as `cargo xtask pins` prints them. CI sets PINGERS_GATEWAY_KEY_PINS and
        // PINGERS_PUSH_RELAY_KEY_PINS (or pass -PpingersGatewayKeyPins=...,
        // -PpingersPushRelayKeyPins=...). With pins set, the core never pins on first use;
        // unset (a local build), it does. A malformed pin fails the build here rather than the
        // app at start.
        fun pins(property: String, variable: String) {
            val value = providers.gradleProperty(property)
                .orElse(providers.environmentVariable(variable))
                .getOrElse("")
                .trim()
            if (value.isNotEmpty()) {
                val pin = Regex("[A-Za-z0-9_-]{43}")
                val bad = value.split(',').map { it.trim() }.filterNot { pin.matches(it) }
                if (bad.isNotEmpty()) {
                    throw GradleException(
                        "$variable must be base64url SHA-256 hashes separated by commas " +
                            "(${bad.size} entr${if (bad.size == 1) "y is" else "ies are"} not)",
                    )
                }
            }
            buildConfigField("String", variable.removePrefix("PINGERS_"), "\"$value\"")
        }
        pins("pingersGatewayKeyPins", "PINGERS_GATEWAY_KEY_PINS")
        pins("pingersPushRelayKeyPins", "PINGERS_PUSH_RELAY_KEY_PINS")
    }

    testOptions {
        unitTests.isIncludeAndroidResources = true
    }

    // gms: Play services location and FCM alongside UnifiedPush. foss: no Google code at all,
    // UnifiedPush only. Debug builds default to gms when google-services.json is configured.
    // Each flavour has its own application id, so both install side by side: org.pingers.app
    // and org.pingers.app.foss, each with .debug in debug builds. Firebase knows only
    // org.pingers.app; the gms debug build (org.pingers.app.debug) uses that client's
    // configuration, which FCM accepts as long as the Firebase API key is not restricted to
    // the release package (see README, "FCM").
    flavorDimensions += "services"
    productFlavors {
        create("gms") {
            dimension = "services"
            isDefault = googleServicesJson.isPresent
        }
        create("foss") {
            dimension = "services"
            applicationIdSuffix = ".foss"
            isDefault = !googleServicesJson.isPresent
        }
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

    signingConfigs {
        create("release") {
            if (signingProblems.isEmpty()) {
                storeFile = File(releaseKeystorePath!!)
                storeType = "PKCS12"
                storePassword = releaseKeystorePassword
                keyAlias = releaseKeyAlias
                keyPassword = releaseKeystorePassword
            }
        }
    }

    buildTypes {
        debug {
            applicationIdSuffix = ".debug"
            buildConfigField("String", "EXTRA_CA", literal(extraCaPem))
            buildConfigField("String", "DEV_REGISTRATION_CODE", literal(devRegistrationCode))
        }
        release {
            // Never a test CA in a release, whatever the environment says.
            buildConfigField("String", "EXTRA_CA", "\"\"")
            // Nor a registration code: a release registers with the code from an invite link.
            buildConfigField("String", "DEV_REGISTRATION_CODE", "\"\"")
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            // The upload key or nothing: without it checkReleaseSigning fails the build.
            if (signingProblems.isEmpty()) signingConfig = signingConfigs.getByName("release")
        }
    }

    buildFeatures {
        compose = true
        buildConfig = true
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

// Every release variant checks for the signing key first, so a release build without it stops
// before cargo or the compiler run, with the reason.
tasks.configureEach {
    if (Regex("pre\\w*ReleaseBuild").matches(name)) dependsOn(checkReleaseSigning)
}
cargoNdkRelease.configure { dependsOn(checkReleaseSigning) }

androidComponents {
    onVariants { variant ->
        val native = if (variant.buildType == "release") cargoNdkRelease else cargoNdkDebug
        variant.sources.jniLibs?.addGeneratedSourceDirectory(native, CargoTask::outDir)
        variant.sources.kotlin?.addGeneratedSourceDirectory(uniffiBindings, CargoTask::outDir)
        if (variant.flavorName == "gms" && googleServicesJson.isPresent) {
            val name = variant.name.replaceFirstChar { it.uppercase() }
            val task = tasks.register<GoogleServicesTask>("googleServices$name") {
                description = "Writes Firebase's resources for $name from google-services.json."
                json.set(googleServicesJson)
                packageName.set(firebasePackage)
                resDir.set(layout.buildDirectory.dir("generated/google-services/${variant.name}/res"))
                copy.set(layout.buildDirectory.file("generated/google-services/${variant.name}/google-services.json"))
            }
            variant.sources.res?.addGeneratedSourceDirectory(task, GoogleServicesTask::resDir)
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
    implementation(libs.maplibre)
    testImplementation(libs.junit)
    debugImplementation(libs.compose.ui.tooling)

    // Platform layer.
    implementation(libs.unifiedpush.connector)
    implementation(libs.work.runtime)
    "gmsImplementation"(libs.play.services.location)
    "gmsImplementation"(libs.firebase.messaging)

    testImplementation(libs.junit)
    testImplementation(libs.robolectric)
    testImplementation(libs.androidx.test.core)
    testImplementation(libs.work.testing)
    androidTestImplementation(libs.androidx.test.ext.junit)
    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.androidx.test.core)
}
