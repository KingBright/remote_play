plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

val releaseKeystorePath = providers.environmentVariable("WEAVELO_ANDROID_KEYSTORE").orNull
val releaseKeystorePassword = providers.environmentVariable("WEAVELO_ANDROID_KEYSTORE_PASSWORD").orNull
val releaseKeyAlias = providers.environmentVariable("WEAVELO_ANDROID_KEY_ALIAS").orNull
val releaseKeyPassword = providers.environmentVariable("WEAVELO_ANDROID_KEY_PASSWORD").orNull
val releaseSigningReady = listOf(
    releaseKeystorePath,
    releaseKeystorePassword,
    releaseKeyAlias,
    releaseKeyPassword,
).all { !it.isNullOrBlank() }

android {
    namespace = "fun.hackerlife.weavelo"
    compileSdk = 35

    defaultConfig {
        applicationId = "fun.hackerlife.weavelo"
        minSdk = 26
        targetSdk = 35
        versionCode = 10
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        versionName = "2.0.0-alpha.9"

        ndk {
            abiFilters.addAll(setOf("arm64-v8a"))
        }
    }

    signingConfigs {
        if (releaseSigningReady) {
            create("release") {
                storeFile = file(requireNotNull(releaseKeystorePath))
                storePassword = releaseKeystorePassword
                keyAlias = releaseKeyAlias
                keyPassword = releaseKeyPassword
                enableV1Signing = true
                enableV2Signing = true
            }
        }
    }

    buildTypes {
        release {
            isDebuggable = false
            isJniDebuggable = false
            isMinifyEnabled = false
            signingConfig = signingConfigs.findByName("release")
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
    packaging {
        jniLibs.excludes.add("**/libremote_play_android.so")
        // The release gate binds the exact original native producer bytes.
        // AGP must not silently strip and replace that verified ELF.
        jniLibs.keepDebugSymbols.add("**/libweavelo_android.so")
    }
    buildFeatures {
        compose = true
    }
}

// onlyIf runs before Gradle cleans outputDirectory; doFirst would be too late.
// The formal wrapper redirects only APK output to its public-owned temporary.
tasks.withType<com.android.build.gradle.tasks.PackageAndroidArtifact>().configureEach {
    if (name == "packageRelease") onlyIf {
        val helper = rootProject.projectDir.parentFile.resolve("scripts/release_artifact_lifecycle.py")
        val checked = project.exec {
            commandLine("python3", "-B", helper.absolutePath, "check-current",
                "--apk-output-directory", outputDirectory.get().asFile.absolutePath)
            isIgnoreExitValue = true
        }
        if (checked.exitValue != 0) {
            throw GradleException("Use scripts/build_android_release.sh; an active verified current-output transaction is required")
        }
        true
    }
}

dependencies {
    androidTestImplementation(platform("androidx.compose:compose-bom:2024.02.00"))
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")
    androidTestImplementation("androidx.test.ext:junit:1.1.5")
    androidTestImplementation("androidx.test:runner:1.5.2")
    debugImplementation("androidx.compose.ui:ui-test-manifest")
    testImplementation("junit:junit:4.13.2")
    implementation("androidx.core:core-ktx:1.12.0")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.7.0")
    implementation("androidx.activity:activity-compose:1.8.2")
    implementation(platform("androidx.compose:compose-bom:2024.02.00"))
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-graphics")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.compose.material3:material3")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.8.0")
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.6.3")
    implementation("androidx.camera:camera-core:1.4.2")
    implementation("androidx.camera:camera-camera2:1.4.2")
    implementation("androidx.camera:camera-lifecycle:1.4.2")
    implementation("androidx.camera:camera-view:1.4.2")
    implementation("com.google.zxing:core:3.5.3")
}
