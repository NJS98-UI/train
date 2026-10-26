plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.example.traework2api"
    compileSdk = 36
    buildToolsVersion = "36.0.0"

    defaultConfig {
        applicationId = "com.example.traework2api"
        minSdk = 24
        targetSdk = 36
        versionCode = 64
        versionName = "0.7.33"
    }

    // 固定签名：debug / release 共用仓库内 keystore，保证任何机器 / CI 构建出的 APK 签名一致。
    // 这是"自用不上架"的 key，私钥随本私有仓库提交；换设备/重装不再因签名漂移而丢登录。
    // 注意：它救不回更早用「其它签名」装的旧 App（签名不同无法覆盖安装，需卸载重装并重登一次）。
    signingConfigs {
        create("app") {
            storeFile = file("keystore/traework2api.jks")
            storePassword = "traework2api"
            keyAlias = "traework2api"
            keyPassword = "traework2api"
        }
    }

    buildTypes {
        debug {
            signingConfig = signingConfigs.getByName("app")
        }
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            signingConfig = signingConfigs.getByName("app")
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
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
}
