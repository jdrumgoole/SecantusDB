"""Dev-only tooling. Not shipped — excluded from the sdist and the wheel.

A package rather than a loose directory so `tools.provenance` is importable from
anything that runs with the repo root on `sys.path`: the invoke tasks, the
scripts under `scripts/`, and `tests/conftest.py`.
"""
