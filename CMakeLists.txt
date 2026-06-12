cmake_minimum_required(VERSION 3.21)
project(secantus_wheel C CXX)

# -----------------------------------------------------------------------------
# Option A: build the vendored WiredTiger source + Python bindings as an
# ExternalProject and copy the produced extension + generated Python module
# into the wheel's site-packages layout.
#
# WT's own CMake hard-codes CMAKE_SOURCE_DIR for its include() calls (e.g.,
# cmake/configs/modes.cmake), which means add_subdirectory(vendor/wiredtiger)
# fails because CMAKE_SOURCE_DIR resolves to OUR project root, not WT's. The
# clean fix is to drive WT's build as a separate CMake invocation via
# ExternalProject_Add.
# -----------------------------------------------------------------------------

include(ExternalProject)
include(GNUInstallDirs)

set(WT_SOURCE_DIR  ${CMAKE_CURRENT_SOURCE_DIR}/vendor/wiredtiger)
set(WT_BINARY_DIR  ${CMAKE_CURRENT_BINARY_DIR}/wt-build)
set(WT_INSTALL_DIR ${CMAKE_CURRENT_BINARY_DIR}/wt-install)

# mongodb-7.0.33's CMake adds -Werror at the WT-target level (cmake/strict/
# strict_flags_helpers.cmake). Modern Clang (>= 21) added several warnings
# that flip those targets red — -Wreserved-identifier on WT's __wt_* typedef
# names, -Wimplicit-void-ptr-cast on `(NULL)` returns, etc. Wrapper-level
# CMAKE_C_FLAGS injection is overridden by WT's per-target add_compile_options,
# so suppression has to happen inside WT. We comment out the -Werror lines
# via cmake/patch_wt_strict.py at PATCH_COMMAND time (idempotent, in-tree
# script). The real warnings still print; they just don't fail the build.

set(WT_PATCH_SCRIPT ${CMAKE_CURRENT_SOURCE_DIR}/cmake/patch_wt_strict.py)
set(WT_STRICT_HELPERS ${WT_SOURCE_DIR}/cmake/strict/strict_flags_helpers.cmake)
set(WT_PATCH_PYTHON_SCRIPT ${CMAKE_CURRENT_SOURCE_DIR}/cmake/patch_wt_python.py)
set(WT_PYTHON_CMAKE ${WT_SOURCE_DIR}/lang/python/CMakeLists.txt)
set(WT_PATCH_HELPERS_SCRIPT ${CMAKE_CURRENT_SOURCE_DIR}/cmake/patch_wt_helpers.py)
set(WT_HELPERS_CMAKE ${WT_SOURCE_DIR}/cmake/helpers.cmake)
set(WT_PATCH_MUSL_SCRIPT ${CMAKE_CURRENT_SOURCE_DIR}/cmake/patch_wt_musl.py)
set(WT_OS_FS_C ${WT_SOURCE_DIR}/src/os_posix/os_fs.c)

# Python C extension filename: .so on POSIX, .pyd on Windows. WT only sets the
# Darwin SUFFIX itself; the Windows branch is added by patch_wt_python.py so
# Python's import machinery (which only looks for .pyd) can find the module.
if(WIN32)
    set(WT_PYEXT_NAME "_wiredtiger.pyd")
else()
    set(WT_PYEXT_NAME "_wiredtiger.so")
endif()

ExternalProject_Add(wiredtiger_ext
    SOURCE_DIR        ${WT_SOURCE_DIR}
    BINARY_DIR        ${WT_BINARY_DIR}
    INSTALL_DIR       ${WT_INSTALL_DIR}
    # ExternalProject inherits the parent generator unless CMAKE_GENERATOR is
    # set explicitly (per-arg, NOT via -G in CMAKE_ARGS — that's silently
    # ignored). On Windows the parent uses MSBuild by default, which puts
    # build outputs under a per-config subdir (lang/python/Release/...) and
    # breaks our install paths. Force Ninja everywhere for a uniform layout.
    CMAKE_GENERATOR   Ninja
    PATCH_COMMAND     ${Python3_EXECUTABLE} ${WT_PATCH_SCRIPT} ${WT_STRICT_HELPERS}
              COMMAND ${Python3_EXECUTABLE} ${WT_PATCH_PYTHON_SCRIPT} ${WT_PYTHON_CMAKE}
              COMMAND ${Python3_EXECUTABLE} ${WT_PATCH_HELPERS_SCRIPT} ${WT_HELPERS_CMAKE}
              COMMAND ${Python3_EXECUTABLE} ${WT_PATCH_MUSL_SCRIPT} ${WT_OS_FS_C}
    CMAKE_ARGS
        -DCMAKE_BUILD_TYPE=Release
        -DCMAKE_INSTALL_PREFIX=${WT_INSTALL_DIR}
        -DCMAKE_POSITION_INDEPENDENT_CODE=ON
        -DENABLE_STATIC=ON
        -DWITH_PIC=ON
        -DENABLE_SHARED=OFF
        -DENABLE_PYTHON=ON
        # Belt-and-braces with BUILD_COMMAND below; ENABLE_CPPSUITE=OFF
        # also short-circuits the configure-time SWIG/Catch2 lookups.
        -DENABLE_CPPSUITE=OFF
        -DPython3_EXECUTABLE=${Python3_EXECUTABLE}
    # Build only the SWIG Python module (CMake pulls in wiredtiger_static
    # and the compression-extension shared libs via target dependencies).
    # WT's tests, benchmarks, examples, and simulator targets stay unbuilt.
    # That's a faster build and — more importantly — sidesteps missing-#include
    # bugs in WT's test/bench code that gcc 10 (manylinux's devtoolset)
    # tolerates but modern gcc on musllinux rejects (e.g. <string>, <cstdint>
    # transitive includes that libstdc++ 13+ no longer provides for free).
    BUILD_COMMAND ${CMAKE_COMMAND} --build . --target wiredtiger_python
    # Our outer install() rules read directly from WT_BINARY_DIR; no need
    # for WT's own `cmake --install` step. Skipping it also avoids errors
    # from WT install rules that reference targets we deliberately didn't build.
    INSTALL_COMMAND ${CMAKE_COMMAND} -E echo "skipping WT install (handled by outer wheel install)"
    BUILD_BYPRODUCTS
        ${WT_BINARY_DIR}/lang/python/${WT_PYEXT_NAME}
        ${WT_BINARY_DIR}/lang/python/wiredtiger/swig_wiredtiger.py
    BUILD_ALWAYS OFF
)

# scikit-build-core picks up `install(...)` directives and copies the named
# files into the wheel's platlib at build time. The custom command below
# forces the ExternalProject to run before the install step.
add_custom_target(wt_python_outputs ALL
    DEPENDS wiredtiger_ext
)

# Lay out the wiredtiger Python package inside the wheel:
#   <site-packages>/wiredtiger/__init__.py        (build dir; copied from init.py during WT build)
#   <site-packages>/wiredtiger/swig_wiredtiger.py (build dir; generated by SWIG)
#   <site-packages>/wiredtiger/_wiredtiger.so     (build dir; compiled extension)
#   <site-packages>/wiredtiger/{fpacking,intpacking,packing,packutil}.py (source dir; not copied by WT)
install(FILES
    ${WT_BINARY_DIR}/lang/python/${WT_PYEXT_NAME}
    DESTINATION wiredtiger
    OPTIONAL
)
install(FILES
    ${WT_BINARY_DIR}/lang/python/wiredtiger/__init__.py
    ${WT_BINARY_DIR}/lang/python/wiredtiger/swig_wiredtiger.py
    DESTINATION wiredtiger
    OPTIONAL
)
install(DIRECTORY ${WT_SOURCE_DIR}/lang/python/wiredtiger/
    DESTINATION wiredtiger
    FILES_MATCHING
        PATTERN "*.py"
        PATTERN "init.py" EXCLUDE
)

# -----------------------------------------------------------------------------
# Optional: the Rust storage engine extension (`_secantus_storage`).
#
# This is the "bundle behind a build flag" packaging (chosen over a separate
# companion wheel): the extension links the SAME vendored WiredTiger this wheel
# already builds above (no second WT build), so when the flag is ON it ships
# inside the `secantus` wheel. It is OFF by default — the pure-Python storage
# path is the shipping default until engine-selection (Phase 4+) makes the Rust
# storage engine selectable. With the flag OFF, the wheel is byte-for-byte the
# same as before and the build needs no Rust/clang toolchain.
#
# `crates/secantus-wt/build.rs` resolves WiredTiger from SECANTUS_WT_INCLUDE /
# SECANTUS_WT_LIB (set below to this CMake build's WT output dir), and bindgen
# needs libclang (set LIBCLANG_PATH in the build environment if it isn't auto-
# discovered). cargo builds the PyO3 abi3 cdylib; we rename it to the platform's
# Python-extension filename and install it at the wheel root so `import
# _secantus_storage` resolves.
# -----------------------------------------------------------------------------
option(SECANTUS_BUILD_STORAGE_ENGINE
    "Build the Rust storage extension (_secantus_storage) against the bundled \
WiredTiger and ship it in the wheel (OFF by default; needs a Rust + libclang \
toolchain when ON)."
    OFF)

if(SECANTUS_BUILD_STORAGE_ENGINE)
    find_program(CARGO_EXECUTABLE cargo REQUIRED)

    set(STORAGE_CRATE_DIR  ${CMAKE_CURRENT_SOURCE_DIR}/crates/secantus-storage-py)
    set(STORAGE_TARGET_DIR ${CMAKE_CURRENT_BINARY_DIR}/storage-target)

    # cdylib output name (cargo) vs the Python-importable extension filename we
    # install. abi3 means one filename per platform covers every CPython >=3.10.
    if(WIN32)
        set(STORAGE_CARGO_OUT "_secantus_storage.dll")
        set(STORAGE_EXT_NAME  "_secantus_storage.pyd")
    elseif(APPLE)
        set(STORAGE_CARGO_OUT "lib_secantus_storage.dylib")
        set(STORAGE_EXT_NAME  "_secantus_storage.abi3.so")
    else()
        set(STORAGE_CARGO_OUT "lib_secantus_storage.so")
        set(STORAGE_EXT_NAME  "_secantus_storage.abi3.so")
    endif()

    set(STORAGE_EXT_BUILT  ${STORAGE_TARGET_DIR}/release/${STORAGE_CARGO_OUT})
    set(STORAGE_EXT_STAGED ${CMAKE_CURRENT_BINARY_DIR}/${STORAGE_EXT_NAME})

    # Point the crate's build.rs at THIS build's WiredTiger (built above by the
    # wiredtiger_ext ExternalProject). `cmake -E env` inherits the rest of the
    # environment (PATH, LIBCLANG_PATH, RUSTUP_* ...), so libclang discovery and
    # the rust toolchain are picked up from the build environment.
    #
    # A custom TARGET (not add_custom_command OUTPUT): the command must run on
    # every build so cargo's own dependency tracking decides freshness. The
    # OUTPUT form had no DEPENDS on the crate sources, so once the staged file
    # existed, ninja skipped cargo entirely and editable rebuilds shipped a
    # STALE extension. copy_if_different keeps the no-change case cheap.
    add_custom_target(secantus_storage_ext ALL
        COMMAND ${CMAKE_COMMAND} -E env
            SECANTUS_WT_INCLUDE=${WT_BINARY_DIR}/include
            SECANTUS_WT_LIB=${WT_BINARY_DIR}
            CARGO_TARGET_DIR=${STORAGE_TARGET_DIR}
            ${CARGO_EXECUTABLE} build --release
                --manifest-path ${STORAGE_CRATE_DIR}/Cargo.toml
        COMMAND ${CMAKE_COMMAND} -E copy_if_different ${STORAGE_EXT_BUILT} ${STORAGE_EXT_STAGED}
        DEPENDS wiredtiger_ext
        COMMENT "Building Rust storage extension (_secantus_storage) against bundled WiredTiger"
        VERBATIM
    )

    # Wheel root, alongside the other top-level extensions — `import
    # _secantus_storage` resolves from site-packages.
    install(FILES ${STORAGE_EXT_STAGED} DESTINATION . OPTIONAL)

    # -------------------------------------------------------------------------
    # The Rust server extension (`_secantus_server`, R6) — the embedded Python
    # lifecycle handle over the standalone Rust server. It links the SAME
    # vendored WiredTiger (transitively via secantus-storage-adapter →
    # secantus-storage), so it builds under the same flag, the same way as
    # `_secantus_storage` above. `import _secantus_server` → `RustServer`.
    # -------------------------------------------------------------------------
    set(SERVER_CRATE_DIR  ${CMAKE_CURRENT_SOURCE_DIR}/crates/secantus-server-py)
    set(SERVER_TARGET_DIR ${CMAKE_CURRENT_BINARY_DIR}/server-target)

    if(WIN32)
        set(SERVER_CARGO_OUT "_secantus_server.dll")
        set(SERVER_EXT_NAME  "_secantus_server.pyd")
    elseif(APPLE)
        set(SERVER_CARGO_OUT "lib_secantus_server.dylib")
        set(SERVER_EXT_NAME  "_secantus_server.abi3.so")
    else()
        set(SERVER_CARGO_OUT "lib_secantus_server.so")
        set(SERVER_EXT_NAME  "_secantus_server.abi3.so")
    endif()

    set(SERVER_EXT_BUILT  ${SERVER_TARGET_DIR}/release/${SERVER_CARGO_OUT})
    set(SERVER_EXT_STAGED ${CMAKE_CURRENT_BINARY_DIR}/${SERVER_EXT_NAME})

    # Same always-run custom-target shape as secantus_storage_ext above —
    # see the staleness note there.
    add_custom_target(secantus_server_ext ALL
        COMMAND ${CMAKE_COMMAND} -E env
            SECANTUS_WT_INCLUDE=${WT_BINARY_DIR}/include
            SECANTUS_WT_LIB=${WT_BINARY_DIR}
            CARGO_TARGET_DIR=${SERVER_TARGET_DIR}
            ${CARGO_EXECUTABLE} build --release
                --manifest-path ${SERVER_CRATE_DIR}/Cargo.toml
        COMMAND ${CMAKE_COMMAND} -E copy_if_different ${SERVER_EXT_BUILT} ${SERVER_EXT_STAGED}
        DEPENDS wiredtiger_ext
        COMMENT "Building Rust server extension (_secantus_server) against bundled WiredTiger"
        VERBATIM
    )
    install(FILES ${SERVER_EXT_STAGED} DESTINATION . OPTIONAL)
endif()
