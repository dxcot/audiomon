plugins {
    id("com.android.application")
}

android {
    namespace = "dev.audiorast"
    compileSdk = 36
    ndkVersion = "30.0.16248370"

    defaultConfig {
        applicationId = "dev.audiorast"
        minSdk = 28
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
        ndk {
            abiFilters += listOf("arm64-v8a", "armeabi-v7a")
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            // Debug-signed until there is a release keystore.
            signingConfig = signingConfigs.getByName("debug")
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
        "build", "--release", "-p", "audiorast-receiver",
    )
}

tasks.named("preBuild") { dependsOn(cargoBuild) }
