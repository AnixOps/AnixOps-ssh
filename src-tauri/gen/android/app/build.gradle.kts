import java.util.Properties
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

// Release signing (RELEASING.md, "Mobile"). The release workflow decodes the
// upload keystore to a file and passes these four variables. When any is
// missing (forks, pull requests, local builds) no signing config is created
// and the release build comes out unsigned, exactly as before. Nothing secret
// lives in this repository.
val releaseKeystorePath: String? = System.getenv("ANDROID_KEYSTORE_PATH")?.takeIf { it.isNotBlank() }
val releaseKeystorePassword: String? = System.getenv("ANDROID_KEYSTORE_PASSWORD")?.takeIf { it.isNotEmpty() }
val releaseKeyAlias: String? = System.getenv("ANDROID_KEY_ALIAS")?.takeIf { it.isNotBlank() }
val releaseKeyPassword: String? = System.getenv("ANDROID_KEY_PASSWORD")?.takeIf { it.isNotEmpty() }
val releaseSigningAvailable: Boolean =
    releaseKeystorePath != null &&
        file(releaseKeystorePath).isFile &&
        releaseKeystorePassword != null &&
        releaseKeyAlias != null &&
        releaseKeyPassword != null

android {
    compileSdk = 37
    namespace = "com.anixops.ssh"
    defaultConfig {
        manifestPlaceholders["usesCleartextTraffic"] = "false"
        applicationId = "com.anixops.ssh"
        minSdk = 24
        targetSdk = 37
        versionCode = tauriProperties.getProperty("tauri.android.versionCode", "1").toInt()
        versionName = tauriProperties.getProperty("tauri.android.versionName", "1.0")
    }
    signingConfigs {
        if (releaseSigningAvailable) {
            create("release") {
                storeFile = file(releaseKeystorePath!!)
                storePassword = releaseKeystorePassword
                keyAlias = releaseKeyAlias
                keyPassword = releaseKeyPassword
            }
        }
    }
    buildTypes {
        getByName("debug") {
            manifestPlaceholders["usesCleartextTraffic"] = "true"
            isDebuggable = true
            isJniDebuggable = true
            isMinifyEnabled = false
            packaging {
                jniLibs.keepDebugSymbols.add("*/arm64-v8a/*.so")
                jniLibs.keepDebugSymbols.add("*/armeabi-v7a/*.so")
                jniLibs.keepDebugSymbols.add("*/x86/*.so")
                jniLibs.keepDebugSymbols.add("*/x86_64/*.so")
            }
        }
        getByName("release") {
            if (releaseSigningAvailable) {
                signingConfig = signingConfigs.getByName("release")
            }
            optimization {
               enable = true
            }
            proguardFiles(
                *fileTree(".") {
                  include("**/*.pro")
                  exclude("build/**")
                }.files.toTypedArray()
            )
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    buildFeatures {
        buildConfig = true
    }
}

kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_1_8
    }
}

rust {
    rootDirRel = "../../../"
}

// Version of rustls-platform-verifier-android in the workspace's Cargo.lock
// (four levels up from this module).
val rustlsVerifierVersion: Provider<String> =
    providers.fileContents(layout.projectDirectory.file("../../../../Cargo.lock")).asText.map { text ->
        val lines = text.lines()
        val at = lines.indexOfFirst { it.trim() == "name = \"rustls-platform-verifier-android\"" }
        check(at >= 0) { "rustls-platform-verifier-android not found in Cargo.lock" }
        lines.drop(at + 1).first { it.trimStart().startsWith("version = ") }
            .substringAfter('"').substringBefore('"')
    }

dependencies {
    // The Kotlin half of rustls-platform-verifier (repository in the root
    // build file). Its version must equal the Rust crate's, so it is read from
    // Cargo.lock, as the crate's README prescribes. The archive publishes it
    // under org/rustls/rustls-platform-verifier/<version> (0.1.0, 0.1.1 and
    // 0.2.0 as of November 2026).
    implementation("org.rustls:rustls-platform-verifier:${rustlsVerifierVersion.get()}")
    implementation("androidx.webkit:webkit:1.14.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.lifecycle:lifecycle-process:2.10.0")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
}

apply(from = file("tauri.build.gradle.kts"))
