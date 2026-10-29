plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "ai.storyteller.cadcraft"
    compileSdk = 34
    ndkVersion = "26.1.10909125"

    defaultConfig {
        applicationId = "ai.storyteller.cadcraft"
        minSdk = 26
        targetSdk = 34
        versionCode = 1400
        versionName = "0.14.0"

        ndk {
            abiFilters += listOf("arm64-v8a")
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("debug")
        }
        debug {
            isJniDebuggable = true
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    sourceSets {
        getByName("main") {
            jniLibs.srcDirs("src/main/jniLibs")
        }
    }
}

val syncLauncherIcons by tasks.registering {
    description = "Copies CADCraft launcher icons from assets/app-icon into Android mipmap directories"
    doLast {
        val iconRoot = rootProject.projectDir.resolve("../../assets/app-icon")
        val resRoot = projectDir.resolve("src/main/res")
        val densityMap = mapOf(
            "mipmap-mdpi" to iconRoot.resolve("hicolor/48x48/apps/ai.storyteller.cadcraft.png"),
            "mipmap-hdpi" to iconRoot.resolve("cadcraft-64.png"),
            "mipmap-xhdpi" to iconRoot.resolve("hicolor/128x128/apps/ai.storyteller.cadcraft.png"),
            "mipmap-xxhdpi" to iconRoot.resolve("cadcraft-256.png"),
            "mipmap-xxxhdpi" to iconRoot.resolve("hicolor/512x512/apps/ai.storyteller.cadcraft.png")
        )
        for ((dirName, srcFile) in densityMap) {
            if (srcFile.exists()) {
                val targetDir = resRoot.resolve(dirName)
                targetDir.mkdirs()
                srcFile.copyTo(targetDir.resolve("ic_launcher.png"), overwrite = true)
            }
        }
    }
}

tasks.named("preBuild") {
    dependsOn(syncLauncherIcons)
}
