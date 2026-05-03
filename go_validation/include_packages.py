"""In-scope Go test packages under vendor/mongo-go-driver/.

Conservative starting set: BSON serialization (server-independent;
catches wire-format regressions) and the integration package (server-
dependent; many tests will self-skip on topology requirements but the
ones that survive give us "Go driver actually works against SecantusDB"
coverage).

Out of scope:
  ./internal/csfle*       (client-side encryption)
  ./internal/aws*         (Atlas-only auth)
  ./internal/spectest/atlas-data-lake-testing
  ./mongo/options/encryption*
  ./x/network/...         (low-level wire stuff already covered by integration)
"""

from __future__ import annotations

# Package paths relative to the vendor/mongo-go-driver/ module root.
INCLUDE: list[str] = [
    "./bson/...",
    "./mongo",
    # Add ./internal/integration/... when ready to absorb the runtime cost
    # (~several minutes) and the topology-skip noise. Leaving it out of the
    # initial baseline so the first invocation is fast and the gauge is
    # interpretable.
]
