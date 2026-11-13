pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "pingers"

// app: the Compose shell. It draws what pingers-screens publishes and implements
// the Rust core's Platform callbacks; for now it runs on fake data (FakeRepository).
include(":app")
