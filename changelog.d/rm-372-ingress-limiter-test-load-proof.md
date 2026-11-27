### The ingress rate limiter comparison no longer depends on the client's load

#### Fixed

- Fixes RM-372 (`tests/backlog/test_rm_001_028.py::test_ingress_rate_limiter_admits_queues_and_closes`).
  The test compared which of six connections each server answered with the
  limiter at one a second, and it failed in CI twice (pg-batch PR #3036,
  rm-batch snapshot PR #3073) because six client threads under load did not
  connect within one second. The test now stops each server (SIGSTOP) while
  all six connect and send `hello`, then resumes it, so the server accepts
  all six within milliseconds whatever the client's load, and measures every
  wait from the resume in one thread. It passed 6 of 6 under 16 CPU
  burners, and still fails against a server whose limiter admits everything.
  The server's limiter needed no change: it matches mongod 8.2.11.
