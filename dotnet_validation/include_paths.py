"""Curated scope for the mongo-csharp-driver (.NET / C#) conformance gauge.

The C# driver's tests are xUnit, run via ``dotnet test``. ``TEST_PROJECT`` is
the integration-test project the gauge runs (the BSON / pure-unit projects that
never open a connection are not run). ``FILTER`` is the xUnit
``--filter`` expression that selects the in-scope tests and excludes
out-of-scope categories/traits (transactions, CSFLE, Atlas-search, load
balancing, etc.) — each documented inline.

mongo-csharp-driver's tests read the server connection string from the
``MONGODB_URI`` environment variable, so (unlike mongocxx) the gauge can serve
them on an ephemeral port.
"""

from __future__ import annotations

# The integration-test project (path relative to the vendored repo root).
TEST_PROJECT: str = "tests/MongoDB.Driver.Tests/MongoDB.Driver.Tests.csproj"

# Target framework to build/run. The project multi-targets
# netcoreapp3.1 / net6.0 / net10.0; we run net10.0 (matches the installed SDK).
FRAMEWORK: str = "net10.0"

# xUnit ``--filter`` expression scoping the run to the CRUD specification
# conformance suite (``MongoDB.Driver.Tests.Specifications.crud`` — the
# JSON-driven official CRUD spec tests, the core of what SecantusDB implements).
# MongoDB.Driver.Tests as a whole is enormous and dominated by non-server unit
# tests (LINQ provider, BSON serialization) plus feature suites that need
# external services (CSFLE/KMS, auth, Atlas Search, load balancing) or a real
# multi-node deployment (transactions, sessions, SDAM, retryable). The
# ``[RequireServer]`` attribute self-skips version/topology-gated cases within
# the selected set. Broaden this filter to add more spec families (e.g.
# ``read_write_concern``) as they're validated.
FILTER: str = "FullyQualifiedName~MongoDB.Driver.Tests.Specifications.crud"
