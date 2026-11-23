plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.harness.mobile"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.harness.mobile"
        minSdk = 26
        targetSdk = 36
        // CI passes the release tag (-PappVersion=0.1.13 -PappVersionCode=113); local builds keep the defaults.
        versionCode = (findProperty("appVersionCode") as String?)?.toIntOrNull() ?: 1
        versionName = (findProperty("appVersion") as String?) ?: "0.1.0"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
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

    buildFeatures {
        buildConfig = true
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.15.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.swiperefreshlayout:swiperefreshlayout:1.1.0")
    implementation("androidx.security:security-crypto:1.1.0")
    implementation("com.journeyapps:zxing-android-embedded:4.3.0")
}
