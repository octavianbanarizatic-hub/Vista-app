plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.vista.android"
    compileSdk = 36

    defaultConfig {
        minSdk = 24
        consumerProguardFiles("consumer-rules.pro")
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    kotlinOptions {
        jvmTarget = "1.8"
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.9.0")
    implementation("androidx.activity:activity-ktx:1.10.1")
    // The video player, its controls, HLS for live streams, and the media
    // session (lock screen and notification controls, background play).
    implementation("androidx.media3:media3-exoplayer:1.8.0")
    implementation("androidx.media3:media3-exoplayer-hls:1.8.0")
    implementation("androidx.media3:media3-ui:1.8.0")
    implementation("androidx.media3:media3-session:1.8.0")
    implementation(project(":tauri-android"))
    // Checks for new videos in the background (NewVideos).
    implementation("androidx.work:work-runtime-ktx:2.10.1")
    // Python, yt-dlp and QuickJS, packaged as native libraries.
    implementation("io.github.junkfood02.youtubedl-android:library:0.18.1")
}
