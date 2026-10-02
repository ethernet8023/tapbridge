import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.tapbridge.app"
    compileSdk = 35
    ndkVersion = "27.2.12479018"

    defaultConfig {
        applicationId = "dev.tapbridge.app"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"

        ndk {
            abiFilters += listOf("arm64-v8a")
        }
    }

    // Task that stages the prebuilt Rust shared library
    // (libtapbridge_phone.so, arm64-v8a, 16 KB page-aligned) into
    // $buildDir/rustJniLibs so AGP packages it into the APK. The .so is built
    // by the parent workspace: cargo ndk -t arm64-v8a -P26 build -p
    // tapbridge-phone --release --locked, with
    // CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS containing
    // -C link-arg=-Wl,-z,max-page-size=16384, output to target/android-jni.
    val rebuildRoot = rootDir.parentFile
    val rustSoDir = file("$rebuildRoot/target/android-jni/arm64-v8a")
    val jniLibsDir = file("$buildDir/rustJniLibs/arm64-v8a")

    val buildRust = tasks.register<Exec>("buildRust") {
        workingDir(rebuildRoot)
        environment("ANDROID_NDK_HOME", android.sdkDirectory.resolve("ndk/27.2.12479018").absolutePath)
        environment("CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS", "-C link-arg=-Wl,-z,max-page-size=16384")
        commandLine("cargo", "ndk", "-t", "arm64-v8a", "-P", "26", "-o", file("$rebuildRoot/target/android-jni").absolutePath, "build", "-p", "tapbridge-phone", "--release", "--locked")
    }
    tasks.register<Copy>("stageRustJniLib") {
        dependsOn(buildRust)
        from(rustSoDir) { include("libtapbridge_phone.so") }
        into(jniLibsDir)
        inputs.file(file("$rustSoDir/libtapbridge_phone.so"))
    }

    sourceSets["main"].jniLibs.srcDir(jniLibsDir.parentFile)

    tasks.matching { it.name == "mergeDebugJniLibFolders" || it.name == "mergeReleaseJniLibFolders" }.configureEach {
        dependsOn("stageRustJniLib")
    }

    packaging {
        jniLibs {
            // The .so is built by cargo with 16 KB alignment; AGP just packages it.
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }
}

dependencies {
    testImplementation("junit:junit:4.13.2")
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("com.google.android.material:material:1.12.0")
}
