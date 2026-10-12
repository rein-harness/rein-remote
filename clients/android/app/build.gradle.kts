plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.reinharness.remote"
    compileSdk = 35

    defaultConfig {
        applicationId = "dev.reinharness.remote"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.1"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // Signed with the debug key, for measuring and CI only (a debuggable build checks every
            // JNI call and runs slower): `-Prr.testBuildType=release`. Nothing here is published.
            signingConfig = signingConfigs.getByName("debug")
        }
    }
    testBuildType = (project.findProperty("rr.testBuildType") as String?) ?: "debug"
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
}

// The Rust core (crates/rr-jni) goes in src/main/jniLibs: ./build-core.sh first.
val checkCore by tasks.registering {
    val libs = layout.projectDirectory.dir("src/main/jniLibs")
    doLast {
        val found = libs.asFile.walkTopDown().any { it.name == "librr_jni.so" }
        if (!found) throw GradleException("No librr_jni.so in ${libs.asFile}: run clients/android/build-core.sh first")
    }
}
tasks.configureEach {
    if (name.startsWith("merge") && name.endsWith("JniLibFolders")) dependsOn(checkCore)
}

dependencies {
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test:core:1.6.1")
    androidTestImplementation("androidx.test:runner:1.6.2")
    androidTestImplementation("androidx.test:rules:1.6.1")
    androidTestImplementation("androidx.test.ext:junit:1.2.1")
}
