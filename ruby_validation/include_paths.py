"""In-scope RSpec paths under vendor/mongo-ruby-driver/spec/.

Initial baseline: every spec file under ``spec/mongo/`` that
``require 'lite_spec_helper'``. Those don't connect to a server, so
they exercise the BSON / URI / auth-handshake / event /
topology-decoding / error-class / protocol-encoding logic without
needing a running cluster.

Mixing a single ``require 'spec_helper'`` (full-helper) file into
the same rspec run pulls in a global authorized-client setup that
poisons every other spec — so the discovery in ``discover_lite()``
explicitly filters those out. Once the lite gauge is green, full
integration specs that work against a single-node deployment will
be added through a separate include path.
"""

from __future__ import annotations

from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR_SPEC = REPO_ROOT / "vendor" / "mongo-ruby-driver" / "spec"


def _is_lite(path: Path) -> bool:
    """A spec file is "lite" when its first ~5 require lines mention
    only ``lite_spec_helper`` (and not ``spec_helper``)."""
    try:
        lines = path.read_text(errors="ignore").splitlines()[:20]
    except OSError:
        return False
    head = "\n".join(lines)
    if "require 'spec_helper'" in head or 'require "spec_helper"' in head:
        return False
    return "lite_spec_helper" in head


# Lite-spec files that reference ``shared_examples`` defined under
# ``spec/support/shared/`` — those are loaded by the full
# ``spec_helper.rb`` (``Dir['./spec/support/shared/*.rb'].each``),
# not by ``lite_spec_helper.rb``, so the file fails to load under
# the lite-only path. They're nominally lite (require lite helper)
# but in practice need the full helper's load path. Filter them
# out until we add a "lite + shared examples" tier.
_LITE_BUT_NEEDS_SHARED: frozenset[str] = frozenset(
    {
        "spec/mongo/server/monitor/app_metadata_spec.rb",
    }
)


def _scan(base_dir: str) -> list[str]:
    """Return repo-relative paths to lite specs under
    ``spec/<base_dir>/``, filtered through
    ``_LITE_BUT_NEEDS_SHARED``."""
    base = VENDOR_SPEC / base_dir
    if not base.is_dir():
        return []
    out: list[str] = []
    for f in sorted(base.rglob("*_spec.rb")):
        rel = f"spec/{base_dir}/{f.relative_to(base).as_posix()}"
        if rel in _LITE_BUT_NEEDS_SHARED:
            continue
        if _is_lite(f):
            out.append(rel)
    return out


def discover_lite() -> list[str]:
    """Return all lite-spec paths under spec/mongo/ and
    spec/spec_tests/, repo-relative.

    Sorted (per directory) for deterministic test ordering. Excludes
    ``_LITE_BUT_NEEDS_SHARED`` files that nominally use
    ``lite_spec_helper`` but reference shared examples defined in
    files only the full ``spec_helper.rb`` loads.

    ``spec/spec_tests/`` holds the YAML-driven cross-driver conformance
    runners (CRUD, connection-string, server-selection, SDAM,
    auth, max-staleness…) that are also lite-only — they're the
    closest analogue to pymongo's spec tests and lift conformance
    coverage by hundreds of tests on top of the unit-spec baseline.
    """
    return _scan("mongo") + _scan("spec_tests")


# Exposed as ``INCLUDE`` so the runner stays drop-in compatible with
# the static-list shape used by node_validation / go_validation.
INCLUDE: list[str] = discover_lite()

# RSpec ``--tag ~<name>`` patterns to skip slow / env-dependent tests.
SKIP_TAGS: list[str] = []
