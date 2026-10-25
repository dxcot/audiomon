import java.util.Properties

plugins {
    id("com.android.application")
}

// Release signing comes from android/keystore.properties (local only, never committed):
// storeFile, storePassword, keyAlias, keyPassword. Without it, release builds are
// debug-signed so CI and other contributors can still build.
val releaseKeys = Properties().apply {
    val file = rootProject.file("keystore.properties")
    if (file.exists()) file.inputStream().use { load(it) }
}

android {
    namespace = "dev.audiomon"
    compileSdk = 36
    ndkVersion = "30.0.16248370"

    defaultConfig {
        applicationId = "dev.audiomon"
        minSdk = 28
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
        ndk {
            abiFilters += listOf("arm64-v8a", "armeabi-v7a")
        }
    }

    signingConfigs {
        if (!releaseKeys.isEmpty) {
            create("release") {
                storeFile = file(releaseKeys.getProperty("storeFile"))
                storePassword = releaseKeys.getProperty("storePassword")
                keyAlias = releaseKeys.getProperty("keyAlias")
                keyPassword = releaseKeys.getProperty("keyPassword")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            signingConfig = signingConfigs.findByName("release") ?: signingConfigs.getByName("debug")
        }
    }
}

// Builds the Rust engine (crates/receiver) into jniLibs before every Android build.
val cargoBuild = tasks.register<Exec>("cargoBuild") {
    val repoRoot = rootDir.parentFile
    workingDir = repoRoot
    commandLine(
        "cargo", "ndk",
        "-t", "arm64-v8a", "-t", "armeabi-v7a",
        "--platform", "28",
        "-o", file("src/main/jniLibs").absolutePath,
        "build", "--release", "-p", "audiomon-receiver",
    )
}

tasks.named("preBuild") { dependsOn(cargoBuild) }
