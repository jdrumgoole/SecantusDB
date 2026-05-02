cmake_minimum_required(VERSION 3.21)
project(secantus_wheel C CXX)

# -----------------------------------------------------------------------------
# Option A scaffold: build the vendored WiredTiger source + Python bindings,
# install them into the Python site-packages layout that the wheel produces.
#
# This file is a STARTING POINT and will need iteration on real CI runners
# (cibuildwheel runs on macOS x86_64/arm64 + manylinux x86_64/aarch64). Things
# most likely to need tweaking on first run:
#
#   * Static-link config — WT's lang/python build needs either a PIC-static
#     wiredtiger library OR a shared one. For self-contained wheels we want
#     PIC-static so auditwheel/delocate doesn't have to chase a separate
#     libwiredtiger.{so,dylib}.
#   * Install destination — scikit-build-core installs into the wheel's
#     ``platlib``; the WT build emits ``_wiredtiger.so`` and a generated
#     ``wiredtiger.py`` which need to land at ``<platlib>/wiredtiger/`` so
#     ``import wiredtiger`` works. The exact target names below may need to
#     be adjusted to match WT's actual build outputs.
#   * Disabled subdirectories — WT's full tree builds tools/tests/benchmarks
#     we don't need; ``EXCLUDE_FROM_ALL`` keeps them out of the install.
# -----------------------------------------------------------------------------

# Tell WT to build static-with-PIC and produce the Python SWIG bindings.
set(ENABLE_STATIC ON CACHE BOOL "" FORCE)
set(WITH_PIC ON CACHE BOOL "" FORCE)
set(ENABLE_SHARED OFF CACHE BOOL "" FORCE)
set(ENABLE_PYTHON ON CACHE BOOL "" FORCE)
set(WT_STANDALONE_BUILD OFF CACHE BOOL "" FORCE)

add_subdirectory(vendor/wiredtiger EXCLUDE_FROM_ALL)

# WT's Python bindings emit:
#   - $<TARGET_FILE:wiredtiger_python>   → _wiredtiger.so (compiled extension)
#   - ${WT_PY_BUILD_DIR}/wiredtiger/      → generated Python sources
# Install both into the ``wiredtiger`` package directory inside the wheel.
install(TARGETS wiredtiger_python
    LIBRARY DESTINATION wiredtiger
    RUNTIME DESTINATION wiredtiger)

install(DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}/vendor/wiredtiger/lang/python/wiredtiger/
    DESTINATION wiredtiger
    FILES_MATCHING PATTERN "*.py")

install(DIRECTORY vendor/wiredtiger/lang/python/wiredtiger/
    DESTINATION wiredtiger
    FILES_MATCHING PATTERN "*.py")
