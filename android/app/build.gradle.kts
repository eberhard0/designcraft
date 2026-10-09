plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
}

android {
    namespace = "com.iameberhard.designcraft"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.iameberhard.designcraft"
        minSdk = 30
        targetSdk = 35
        // CI passes the run number / tag through the environment; local builds fall back to 1 / 0.5.0.
        versionCode = System.getenv("VERSION_CODE")?.toIntOrNull() ?: 1
        versionName = System.getenv("VERSION_NAME") ?: "0.4.0"
        // The Rust library is built for arm64 only (`cargo ndk -t arm64-v8a`); see android/README.md.
        ndk { abiFilters += "arm64-v8a" }
    }

    // Upload key supplied by CI through the environment (KEYSTORE_FILE, KEYSTORE_PASSWORD,
    // KEY_ALIAS, KEY_PASSWORD); local builds stay unsigned.
    val keystoreFile = System.getenv("KEYSTORE_FILE")
    signingConfigs {
        if (keystoreFile != null) {
            create("release") {
                storeFile = file(keystoreFile)
                storePassword = System.getenv("KEYSTORE_PASSWORD")
                keyAlias = System.getenv("KEY_ALIAS")
                keyPassword = System.getenv("KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        debug {
            // Lets a debug-signed test build sit next to the signed release on the same phone.
            applicationIdSuffix = ".debug"
        }
        release {
            // Almost everything is native code; the Kotlin shell is tiny and reached from JNI by
            // name, so keep it unshrunk.
            isMinifyEnabled = false
            isShrinkResources = false
            if (keystoreFile != null) signingConfig = signingConfigs.getByName("release")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    buildFeatures {
        // android-activity ships its own GameActivity C++ glue; never link the upstream prefab.
        prefab = false
    }

    packaging {
        jniLibs {
            // Keep the .so uncompressed and page-aligned (16 KB pages on new devices).
            useLegacyPackaging = false
        }
    }

    lint {
        checkReleaseBuilds = false
    }
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.appcompat)
    implementation(libs.androidx.activity.ktx)
    implementation(libs.androidx.games.activity)
}
