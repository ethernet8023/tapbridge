import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
    // Reads app/google-services.json (gitignored, the real Firebase project
    // config) and fails the build with its own message when it is missing.
    // Without this plugin FirebaseApp never initializes and FCM wake is dead.
    id("com.google.gms.google-services")
}

// Single source of truth for versioning.
val tapbridgeProps = Properties().apply {
    val f = rootProject.file("version.properties")
    if (f.exists()) f.inputStream().use { load(it) }
}
val versionCodeValue = (tapbridgeProps.getProperty("versionCode") ?: "2").toInt()
val versionNameValue = tapbridgeProps.getProperty("versionName") ?: "0.2.0"

android {
    namespace = "computer.ethernet.tapbridge"
    compileSdk = 36
    ndkVersion = "27.2.12479018"

    defaultConfig {
        applicationId = "computer.ethernet.tapbridge"
        minSdk = 26
        targetSdk = 36
        versionCode = versionCodeValue
        versionName = versionNameValue

        ndk {
            abiFilters += listOf("arm64-v8a")
        }
    }

    // Upload-key signing reads Gradle properties (values are never printed).
    // The keystore lives outside the repo; when the properties are missing the
    // release build fails at the signing step with Gradle's own message.
    signingConfigs {
        create("upload") {
            val props = Properties().apply {
                val gradleUserHome = System.getenv("GRADLE_USER_HOME")
                    ?: "${System.getProperty("user.home")}/.gradle"
                val f = file("$gradleUserHome/gradle.properties")
                if (f.exists()) f.inputStream().use { load(it) }
            }
            val store = System.getenv("TAPBRIDGE_UPLOAD_STORE_FILE")
                ?: props.getProperty("TAPBRIDGE_UPLOAD_STORE_FILE")
            val alias = System.getenv("TAPBRIDGE_UPLOAD_KEY_ALIAS")
                ?: props.getProperty("TAPBRIDGE_UPLOAD_KEY_ALIAS")
            val storePass = System.getenv("TAPBRIDGE_UPLOAD_STORE_PASSWORD")
                ?: props.getProperty("TAPBRIDGE_UPLOAD_STORE_PASSWORD")
            val keyPass = System.getenv("TAPBRIDGE_UPLOAD_KEY_PASSWORD")
                ?: props.getProperty("TAPBRIDGE_UPLOAD_KEY_PASSWORD")
            if (store != null && alias != null && storePass != null && keyPass != null) {
                val candidates = listOf(
                    rootProject.file(store),
                    rootProject.file("../../" + store),
                    rootProject.file(store.replace('\\', '/').takeLastWhile { it != '/' }),
                )
                storeFile = candidates.firstOrNull { it.exists() } ?: rootProject.file(store)
                storePassword = storePass
                keyAlias = alias
                keyPassword = keyPass
                enableV1Signing = false
                enableV2Signing = true
                enableV3Signing = true
            }
        }
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
            signingConfig = signingConfigs.getByName("upload")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    lint {
        warningsAsErrors = false
        abortOnError = true
    }

    sourceSets {
        getByName("main") {
            jniLibs.srcDir(file("$buildDir/rustJniLibs"))
        }
    }
}

// Task that stages the freshly built Rust shared library
// (libtapbridge_phone.so, arm64-v8a, 16 KB page-aligned) into
// $buildDir/rustJniLibs so AGP packages it into the APK. Built by the parent
// workspace via cargo-ndk with -C link-arg=-Wl,-z,max-page-size=16384, output
// to target/android-jni.
val rebuildRoot = rootDir.parentFile
val rustSoDir = file("$rebuildRoot/target/android-jni/arm64-v8a")
val jniLibsDir = file("$buildDir/rustJniLibs/arm64-v8a")

val buildRust = tasks.register<Exec>("buildRust") {
    workingDir(rebuildRoot)
    val cargoHome = System.getenv("CARGO_HOME") ?: "${System.getProperty("user.home")}/.cargo"
    val cargoBin = if (org.gradle.internal.os.OperatingSystem.current().isWindows) "$cargoHome/bin" else "$cargoHome/bin"
    val pathSep = if (org.gradle.internal.os.OperatingSystem.current().isWindows) ";" else ":"
    environment("PATH", listOf(cargoBin, System.getenv("PATH")).joinToString(pathSep) { it ?: "" })
    environment("ANDROID_NDK_HOME", android.sdkDirectory.resolve("ndk/27.2.12479018").absolutePath)
    environment(
        "CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS",
        "-C link-arg=-Wl,-z,max-page-size=16384"
    )
    val cargo = listOf("$cargoHome/bin/cargo.exe", "$cargoHome/bin/cargo")
        .firstOrNull { file(it).exists() } ?: "cargo"
    commandLine(
        cargo, "ndk", "-t", "arm64-v8a", "-P", "26",
        "-o", file("$rebuildRoot/target/android-jni").absolutePath,
        "build", "-p", "tapbridge-phone", "--release", "--locked"
    )
}
tasks.register<Copy>("stageRustJniLib") {
    dependsOn(buildRust)
    from(rustSoDir) { include("libtapbridge_phone.so") }
    into(jniLibsDir)
    inputs.file(file("$rustSoDir/libtapbridge_phone.so"))
}

tasks.matching { it.name == "mergeDebugJniLibFolders" || it.name == "mergeReleaseJniLibFolders" }.configureEach {
    dependsOn("stageRustJniLib")
}

dependencies {
    testImplementation("junit:junit:4.13.2")
    implementation("androidx.core:core-ktx:1.15.0")
    implementation("androidx.activity:activity-compose:1.9.3")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.8.7")

    val composeBom = platform("androidx.compose:compose-bom:2025.05.00")
    implementation(composeBom)
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    debugImplementation("androidx.compose.ui:ui-tooling")
    debugImplementation("androidx.compose.ui:ui-test-manifest")

    implementation("androidx.camera:camera-core:1.4.2")
    implementation("androidx.camera:camera-camera2:1.4.2")
    implementation("androidx.camera:camera-lifecycle:1.4.2")
    implementation("androidx.camera:camera-view:1.4.2")

    implementation("com.google.mlkit:barcode-scanning:17.3.0")

    // Firebase (FCM wake), aligned via BoM.
    implementation(platform("com.google.firebase:firebase-bom:33.9.0"))
    implementation("com.google.firebase:firebase-messaging-ktx")
}
