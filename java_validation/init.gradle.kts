// Gradle init script — applied to ``mongo-java-driver`` from ``java_validation/runner.py``
// via ``./gradlew --init-script <abs-path-to-this-file>`` so the vendored tree
// stays unmodified.
//
// Overrides ``maxParallelForks`` on every ``Test`` task. The driver's
// ``conventions/testing-base.gradle.kts`` hardcodes ``maxParallelForks = 1``;
// running this init script applies AFTER project evaluation so our override
// wins. Worker count comes from ``SECANTUS_GAUGE_PARALLEL_FORKS`` (set by
// the runner) and falls back to the JVM's reported processor count.
//
// Safety: the bson unit tests are pure encode/decode with no shared static
// state, so JVM-fork-level parallelism is safe. Phase 3 of the driver-
// validation parallelization plan.
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
