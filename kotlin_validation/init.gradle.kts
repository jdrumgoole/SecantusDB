// Gradle init script — applied to ``mongo-java-driver`` from
// ``kotlin_validation/runner.py`` via ``./gradlew --init-script <abs-path>``
// so the vendored tree stays unmodified.
//
// Overrides ``maxParallelForks`` on every ``Test`` task (the driver's
// ``conventions/testing-base.gradle.kts`` hardcodes ``maxParallelForks = 1``);
// running this init script applies AFTER project evaluation so our override
// wins. Worker count comes from ``SECANTUS_GAUGE_PARALLEL_FORKS`` (set by the
// runner) and falls back to the JVM's reported processor count.
//
// Unlike the Java gauge's init script we add no SecantusDB-specific
// ``excludeTestsMatching`` filter — the Kotlin integration include set
// (kotlin_validation/include_modules.py) carries no known-fail invocations
// yet. Add one here if a Kotlin unified-spec scenario needs server-side
// behaviour SecantusDB intentionally doesn't implement.
//
// Uses fully-qualified ``org.gradle.api.tasks.testing.Test`` so the script
// doesn't rely on default Kotlin DSL imports that init scripts may not have.

allprojects {
    afterEvaluate {
        tasks.withType(org.gradle.api.tasks.testing.Test::class.java).configureEach {
            val envForks = System.getenv("SECANTUS_GAUGE_PARALLEL_FORKS")
            val forks = envForks?.toIntOrNull() ?: Runtime.getRuntime().availableProcessors()
            maxParallelForks = forks
            println("[secantus-init] maxParallelForks=$forks for task ${path}")
        }
    }
}
