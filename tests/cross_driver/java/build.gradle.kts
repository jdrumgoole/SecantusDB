plugins {
    id("java")
    id("application")
}

repositories {
    mavenCentral()
}

dependencies {
    implementation("org.mongodb:mongodb-driver-sync:5.2.1")
    implementation("org.slf4j:slf4j-nop:2.0.13")
}

java {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
}

application {
    // Default; the smoke runner overrides via -PmainClass=...
    mainClass.set(project.findProperty("mainClass")?.toString() ?: "com.secantus.smokes.CustomRolesSmoke")
}

// Build a single uber-jar so the pytest runner can spawn smokes
// with `java -cp <jar> <FQN>` without a per-run gradle bootstrap.
tasks.register<Jar>("smokesJar") {
    archiveBaseName.set("secantus-java-smokes")
    archiveClassifier.set("all")
    from(sourceSets.main.get().output)
    dependsOn(configurations.runtimeClasspath)
    from({
        configurations.runtimeClasspath.get()
            .filter { it.name.endsWith("jar") }
            .map { zipTree(it) }
    })
    duplicatesStrategy = DuplicatesStrategy.EXCLUDE
    manifest {
        attributes("Multi-Release" to "true")
    }
}
