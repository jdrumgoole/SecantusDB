"""mongo-kotlin-driver conformance gauge.

Runs the official MongoDB Kotlin driver's integration tests against a
standalone SecantusDB daemon. The Kotlin driver lives inside the
mongo-java-driver monorepo (``driver-kotlin-sync`` /
``driver-kotlin-coroutine``), so this gauge reuses the same vendored
submodule and JVM toolchain (JDK + Gradle wrapper) as the Java gauge —
it just targets the ``:driver-kotlin-sync:integrationTest`` task, where
the wire-exercising tests live (the ``src/test`` tree is pure
Mockito-mocked unit tests that never touch a server).
"""
