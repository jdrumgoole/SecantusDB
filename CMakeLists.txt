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
# The tree we actually patch and build: a private copy inside the build dir.
# The patch scripts rewrite files in place, so patching WT_SOURCE_DIR left the
# submodule permanently dirty in every developer's checkout and coupled patch
# state (source tree) to the ExternalProject stamps (build dir) — see
# cmake/copy_wt_source.cmake for why that coupling is worth removing.
set(WT_WORK_DIR    ${CMAKE_CURRENT_BINARY_DIR}/wt-src)
set(WT_BINARY_DIR  ${CMAKE_CURRENT_BINARY_DIR}/wt-build)
set(WT_INSTALL_DIR ${CMAKE_CURRENT_BINARY_DIR}/wt-install)

# The vendored commit, embedded in the download command so a submodule bump
# invalidates the staged copy (and, through step ordering, the patch and
# configure steps). Falls back to a marker when git is unavailable — an sdist
# has no repo, and there the source cannot change under us anyway.
# ``-C`` into a directory that is not itself a repository makes git walk UP to
# the enclosing one, so with the submodule un-checked-out this silently
# returned the PARENT project's HEAD — a value that changes on every commit,
# which would re-copy and rebuild WiredTiger on each one. Confirm the toplevel
# really is the submodule before trusting the answer.
execute_process(
    COMMAND git -C ${WT_SOURCE_DIR} rev-parse --show-toplevel
    OUTPUT_VARIABLE WT_SRC_TOPLEVEL
    OUTPUT_STRIP_TRAILING_WHITESPACE
    RESULT_VARIABLE WT_SRC_TOP_RC
    ERROR_QUIET
)
set(WT_SRC_SHA "nogit")
if(WT_SRC_TOP_RC EQUAL 0)
    get_filename_component(_wt_top "${WT_SRC_TOPLEVEL}" REALPATH)
    get_filename_component(_wt_src "${WT_SOURCE_DIR}" REALPATH)
    if(_wt_top STREQUAL _wt_src)
        execute_process(
            COMMAND git -C ${WT_SOURCE_DIR} rev-parse HEAD
            OUTPUT_VARIABLE _wt_sha
            OUTPUT_STRIP_TRAILING_WHITESPACE
            RESULT_VARIABLE _wt_sha_rc
            ERROR_QUIET
        )
        if(_wt_sha_rc EQUAL 0 AND NOT _wt_sha STREQUAL "")
            set(WT_SRC_SHA "${_wt_sha}")
        endif()
    endif()
endif()

# mongodb-7.0.33's CMake adds -Werror at the WT-target level (cmake/strict/
# strict_flags_helpers.cmake). Modern Clang (>= 21) added several warnings
# that flip those targets red — -Wreserved-identifier on WT's __wt_* typedef
# names, -Wimplicit-void-ptr-cast on `(NULL)` returns, etc. Wrapper-level
# CMAKE_C_FLAGS injection is overridden by WT's per-target add_compile_options,
# so suppression has to happen inside WT. We comment out the -Werror lines
# via cmake/patch_wt_strict.py at PATCH_COMMAND time (idempotent, in-tree
# script). The real warnings still print; they just don't fail the build.

set(WT_PATCH_SCRIPT ${CMAKE_CURRENT_SOURCE_DIR}/cmake/patch_wt_strict.py)
set(WT_STRICT_HELPERS ${WT_WORK_DIR}/cmake/strict/strict_flags_helpers.cmake)
set(WT_PATCH_PYTHON_SCRIPT ${CMAKE_CURRENT_SOURCE_DIR}/cmake/patch_wt_python.py)
set(WT_PYTHON_CMAKE ${WT_WORK_DIR}/lang/python/CMakeLists.txt)
set(WT_PATCH_HELPERS_SCRIPT ${CMAKE_CURRENT_SOURCE_DIR}/cmake/patch_wt_helpers.py)
set(WT_HELPERS_CMAKE ${WT_WORK_DIR}/cmake/helpers.cmake)
set(WT_PATCH_MUSL_SCRIPT ${CMAKE_CURRENT_SOURCE_DIR}/cmake/patch_wt_musl.py)
set(WT_OS_FS_C ${WT_WORK_DIR}/src/os_posix/os_fs.c)

# The interpreter that runs the patch scripts, handed to the patch step via a
# FILE rather than as a command argument. See the PATCH_COMMAND comment on the
# ExternalProject below: naming ${Python3_EXECUTABLE} in the command made
# ExternalProject's recorded patch text change on every build (PEP 517 isolated
# build envs live in a fresh temp dir each time), which re-ran the patch step
# and rebuilt all of WiredTiger every time. The file's CONTENTS may change
# freely — only the command text is compared — and the patches are idempotent
# text edits whose result does not depend on which interpreter applied them.
# Resolve the interpreter through any symlinks. Under a PEP 517 build
# Python3_EXECUTABLE points into the isolated build environment — e.g.
# ~/.cache/uv/builds-v0/.tmp8zUBBH/bin/python — a path that is DIFFERENT on
# every build. On POSIX that is a symlink to the real interpreter
# (~/.pyenv/versions/3.12.7/bin/python3.12), which is stable, so REALPATH gives
# a value that can be embedded in ExternalProject's recorded commands without
# invalidating them every time.
#
# Building against the resolved base interpreter is equivalent: a venv carries
# no headers or libs of its own, it shares the base interpreter's ABI, and the
# version is unchanged. (On Windows venv pythons are copies rather than
# symlinks, so REALPATH is a no-op there and that platform keeps rebuilding —
# see the note in the ExternalProject below.)
# Ask the interpreter for its BASE interpreter. Under a PEP 517 build
# Python3_EXECUTABLE points into the isolated build environment, whose path
# changes on every build; ``sys._base_executable`` is the stable interpreter
# that venv was created from. This supersedes a plain REALPATH, which only
# worked on POSIX: a Windows venv python is a COPY, not a symlink, so REALPATH
# resolved to the volatile temp path and that platform kept rebuilding.
# REALPATH stays as the fallback for anything that cannot answer.
execute_process(
    COMMAND ${Python3_EXECUTABLE} -c
            "import sys; print(sys._base_executable or sys.executable)"
    OUTPUT_VARIABLE WT_PY_BASE
    OUTPUT_STRIP_TRAILING_WHITESPACE
    RESULT_VARIABLE WT_PY_BASE_RC
    ERROR_QUIET
)
if(WT_PY_BASE_RC EQUAL 0 AND EXISTS "${WT_PY_BASE}")
    set(WT_PY_REAL "${WT_PY_BASE}")
else()
    get_filename_component(WT_PY_REAL "${Python3_EXECUTABLE}" REALPATH)
endif()
message(STATUS "WT patch/configure interpreter: ${WT_PY_REAL}")

set(WT_PATCH_PY_FILE ${CMAKE_CURRENT_BINARY_DIR}/wt_patch_python.txt)
file(WRITE ${WT_PATCH_PY_FILE} "${WT_PY_REAL}")

# Python C extension filename: .so on POSIX, .pyd on Windows. WT only sets the
# Darwin SUFFIX itself; the Windows branch is added by patch_wt_python.py so
# Python's import machinery (which only looks for .pyd) can find the module.
if(WIN32)
    set(WT_PYEXT_NAME "_wiredtiger.pyd")
else()
    set(WT_PYEXT_NAME "_wiredtiger.so")
endif()

# Migration guard for build dirs created before the out-of-source switch.
#
# WiredTiger used to be configured with SOURCE_DIR = vendor/wiredtiger; it is
# now the private copy under this build dir. CMake records the source directory
# in the sub-build's cache, and refuses to reconfigure when it changes:
#
#   CMake Error: The current CMakeCache.txt is different than the one used to
#   generate it ... The source "..." does not match the source "..."
#
# Anyone whose build dir predates that switch therefore hits a hard error whose
# only remedy is deleting build/ — a papercut we can just remove. The WT build
# tree is a derived artifact, so dropping a mismatched one is safe; the cost is
# the rebuild that a stale tree was going to force anyway.
if(EXISTS ${WT_BINARY_DIR}/CMakeCache.txt)
    file(STRINGS ${WT_BINARY_DIR}/CMakeCache.txt _wt_home
         REGEX "^CMAKE_HOME_DIRECTORY:INTERNAL=")
    if(_wt_home)
        string(REPLACE "CMAKE_HOME_DIRECTORY:INTERNAL=" "" _wt_home "${_wt_home}")
        get_filename_component(_wt_home_real "${_wt_home}" REALPATH)
        get_filename_component(_wt_work_real "${WT_WORK_DIR}" REALPATH)
        if(NOT _wt_home_real STREQUAL _wt_work_real)
            message(STATUS
                "WiredTiger build dir was configured for '${_wt_home}', now "
                "'${WT_WORK_DIR}' — removing the stale tree so it reconfigures")
            file(REMOVE_RECURSE ${WT_BINARY_DIR})
            file(REMOVE_RECURSE ${CMAKE_CURRENT_BINARY_DIR}/wiredtiger_ext-prefix)
        endif()
    endif()
endif()

# zlib block-compression is linked into WiredTiger only where libz is available
# (macOS SDK + manylinux/musllinux). Windows has no default libz, so WT builds
# there without the compressor (the storage layer omits the block_compressor
# clause on Windows too — see secantus-storage BOOTSTRAP).
# OFF by default: enabling the extra compressors makes WiredTiger's builtin
# extensions reference libsnappy / liblz4 / libzstd, which then have to be
# present at link time and on every build platform. That is a dependency
# decision, not an experiment knob, so the default build is unchanged.
#
# Turn it ON (with `SECANTUS_WT_EXTRA_COMPRESSORS=1` in the environment for the
# Rust link step) to reproduce the compressor sweep in tasks/backlog.md.
# Profiling the daemon under sustained write load put **65% of server CPU
# inside zlib's `deflate`** on WiredTiger's page-reconciliation path, and
# swapping zlib for lz4 measured +86% throughput and -97% p99.9 on
# incompressible data, +15% / -88% on compressible. mongod defaults to snappy
# for exactly this reason.
# lz4 is the DEFAULT block compressor (see secantus-storage's table configs);
# zlib is built alongside it and is NOT optional, because `block_compressor` is
# recorded per table at create time — a store created before the switch has
# zlib tables that can only be opened while the zlib extension is linked.
#
# snappy and zstd stay opt-in: they measured close to lz4 and adding two more
# link dependencies to every wheel target for no further gain is not worth it.
# Turn them on to reproduce the sweep in tasks/backlog.md.
option(SECANTUS_WT_EXTRA_COMPRESSORS
       "Also build the snappy and zstd block compressors (lz4 + zlib always build)"
       OFF)
if(WIN32)
    set(WT_ZLIB_ARG "")
elseif(SECANTUS_WT_EXTRA_COMPRESSORS)
    set(WT_ZLIB_ARG
        -DENABLE_ZLIB=OFF -DHAVE_BUILTIN_EXTENSION_ZLIB=ON
        -DENABLE_LZ4=OFF -DHAVE_BUILTIN_EXTENSION_LZ4=ON
        -DENABLE_SNAPPY=OFF -DHAVE_BUILTIN_EXTENSION_SNAPPY=ON
        -DENABLE_ZSTD=OFF -DHAVE_BUILTIN_EXTENSION_ZSTD=ON)
else()
    set(WT_ZLIB_ARG
        -DENABLE_ZLIB=OFF -DHAVE_BUILTIN_EXTENSION_ZLIB=ON
        -DENABLE_LZ4=OFF -DHAVE_BUILTIN_EXTENSION_LZ4=ON)
endif()

ExternalProject_Add(wiredtiger_ext
    SOURCE_DIR        ${WT_WORK_DIR}
    # Stage the copy as the DOWNLOAD step so it is stamped and runs once. The
    # vendored commit is in the command text on purpose: bumping the submodule
    # changes the recorded command, which re-copies and — because later steps
    # depend on this one — re-patches and re-configures.
    DOWNLOAD_COMMAND ${CMAKE_COMMAND}
        -DSRC=${WT_SOURCE_DIR}
        -DDST=${WT_WORK_DIR}
        -DWT_SHA=${WT_SRC_SHA}
        -P ${CMAKE_CURRENT_SOURCE_DIR}/cmake/copy_wt_source.cmake
    BINARY_DIR        ${WT_BINARY_DIR}
    INSTALL_DIR       ${WT_INSTALL_DIR}
    # ExternalProject inherits the parent generator unless CMAKE_GENERATOR is
    # set explicitly (per-arg, NOT via -G in CMAKE_ARGS — that's silently
    # ignored). On Windows the parent uses MSBuild by default, which puts
    # build outputs under a per-config subdir (lang/python/Release/...) and
    # breaks our install paths. Force Ninja everywhere for a uniform layout.
    CMAKE_GENERATOR   Ninja
    # Every argument here MUST be stable across builds. ExternalProject records
    # this command verbatim and re-runs the patch step — cascading into
    # configure and a full WiredTiger rebuild — whenever the recorded text
    # differs. Naming ${Python3_EXECUTABLE} directly used to do exactly that:
    # under a PEP 517 build it is the isolated build env's interpreter
    # (~/.cache/uv/builds-v0/.tmpXXXX/bin/python), a fresh temp path on EVERY
    # build, so WT recompiled from scratch every time despite BUILD_ALWAYS OFF.
    # The interpreter is now passed by file (see WT_PATCH_PY_FILE below) so it
    # can vary without invalidating the stamp.
    PATCH_COMMAND ${CMAKE_COMMAND}
        -DPY_FILE=${WT_PATCH_PY_FILE}
        -DSCRIPT_DIR=${CMAKE_CURRENT_SOURCE_DIR}/cmake
        -DWT_SOURCE_DIR=${WT_WORK_DIR}
        -P ${CMAKE_CURRENT_SOURCE_DIR}/cmake/apply_wt_patches.cmake
    CMAKE_ARGS
        -DCMAKE_BUILD_TYPE=Release
        -DCMAKE_INSTALL_PREFIX=${WT_INSTALL_DIR}
        -DCMAKE_POSITION_INDEPENDENT_CODE=ON
        -DENABLE_STATIC=ON
        -DWITH_PIC=ON
        -DENABLE_SHARED=OFF
        -DENABLE_PYTHON=ON
        # zlib block compression, statically linked INTO libwiredtiger as a
        # builtin extension (HAVE_BUILTIN_EXTENSION_ZLIB) so no runtime
        # `extensions=` load is needed and `block_compressor=zlib` on a table
        # create just works. Gated to non-Windows via ${WT_ZLIB_ARG} (below the
        # ExternalProject) — libz ships on macOS and in the manylinux/musllinux
        # images (self-contained), but not by default on Windows. Cuts the
        # per-doc disk-write volume that bounds steady-state write throughput;
        # mongod defaults to snappy, a follow-up can bundle that for the CPU win.
        # (builtin-only — WT rejects ENABLE_<lib> + HAVE_BUILTIN_EXTENSION_<lib>
        # together; the builtin flag both finds the library and statically links
        # it. The paired `-DENABLE_<lib>=OFF` is deliberate belt-and-braces:
        # WT only defaults ENABLE_LZ4 off when the builtin flag is already set,
        # so a build directory configured BEFORE the builtin flag was added —
        # where liblz4 was present and auto-detected — carries ENABLE_LZ4=ON in
        # its CMake cache and the next configure dies on the conflict. Passing
        # it explicitly makes a stale cache harmless; fresh CI containers never
        # saw it, a developer's incremental tree does.)
        ${WT_ZLIB_ARG}
        # Belt-and-braces with BUILD_COMMAND below; ENABLE_CPPSUITE=OFF
        # also short-circuits the configure-time SWIG/Catch2 lookups.
        -DENABLE_CPPSUITE=OFF
        # WT_PY_REAL, not Python3_EXECUTABLE: the raw value is a per-build
        # temp path, and ExternalProject re-runs the CONFIGURE step (then the
        # build) whenever the configure command text changes. Fixing only the
        # PATCH_COMMAND was not enough — the patch stamp then held, but
        # configure still churned through this argument.
        -DPython3_EXECUTABLE=${WT_PY_REAL}
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
install(DIRECTORY ${WT_WORK_DIR}/lang/python/wiredtiger/
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

    # Cargo target-dir root. By default it lives under this build dir, which is
    # per-wheel-tag (build/{wheel_tag}) — so a cibuildwheel job rebuilds the Rust
    # crates from scratch for all 4 CPython × 2 libc wheels (~8×). When
    # SECANTUS_CARGO_TARGET is set (the wheels.yml / publish.yml CI builds point
    # it at a job-persistent, ccache-cached dir), all wheels in a job share one
    # target dir, so the expensive dependency compiles (bindgen et al.) happen
    # once. Each crate still gets its own subdir to avoid cargo target-lock
    # contention when ninja runs the three builds in parallel.
    if(DEFINED ENV{SECANTUS_CARGO_TARGET})
        set(CARGO_TARGET_ROOT $ENV{SECANTUS_CARGO_TARGET})
    else()
        set(CARGO_TARGET_ROOT ${CMAKE_CURRENT_BINARY_DIR})
    endif()

    set(STORAGE_CRATE_DIR  ${CMAKE_CURRENT_SOURCE_DIR}/crates/secantus-storage-py)
    set(STORAGE_TARGET_DIR ${CARGO_TARGET_ROOT}/storage-target)

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
    set(SERVER_TARGET_DIR ${CARGO_TARGET_ROOT}/server-target)

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

    # Profile-guided optimization for the embedded extension (approach #2:
    # a checked-in profile). A committed, sparse profile
    # (crates/pgo/_secantus_server.profdata.tar.gz, regenerated by
    # `invoke rust-pgo-refresh`) is extracted here and fed to the release build
    # as `-Cprofile-use`, on top of the crate's thin-LTO profile. Measured
    # ~12-19% on the write/aggregate paths over LTO alone (tasks/rust-perf-findings.md
    # Finding 8). The profile is function-keyed, so an arm64-generated profile
    # still helps the x86_64/musl wheels (LLVM tolerates the mismatch with
    # `-pgo-warn-missing-function`), just less than an on-target one.
    #   * `SECANTUS_PGO_GENERATE=<dir>` (set by the refresh task's stage 1) flips
    #     this to `-Cprofile-generate`, ignoring any committed profile.
    #   * `SECANTUS_PGO_DISABLE=1` turns PGO off entirely (escape hatch).
    set(SERVER_PGO_GZ ${CMAKE_CURRENT_SOURCE_DIR}/crates/pgo/_secantus_server.profdata.tar.gz)
    set(SERVER_ENV_EXTRA)
    # The PGO branches below hand cargo a RUSTFLAGS via `cmake -E env`, which
    # REPLACES the ambient value — but the wheel builds rely on ambient
    # RUSTFLAGS (cibuildwheel's Linux env sets `-Ctarget-feature=-crt-static`,
    # without which the musllinux target cannot produce a cdylib at all). So
    # compose: ambient flags first, PGO flags appended. An empty ambient value
    # composes to exactly the old behaviour.
    if(DEFINED ENV{RUSTFLAGS} AND NOT "$ENV{RUSTFLAGS}" STREQUAL "")
        set(SERVER_AMBIENT_RUSTFLAGS "$ENV{RUSTFLAGS} ")
    else()
        set(SERVER_AMBIENT_RUSTFLAGS "")
    endif()
    if(DEFINED ENV{SECANTUS_PGO_DISABLE})
        message(STATUS "PGO disabled for _secantus_server (SECANTUS_PGO_DISABLE set)")
    elseif(DEFINED ENV{SECANTUS_PGO_GENERATE})
        message(STATUS "PGO: instrumenting _secantus_server (profile-generate)")
        list(APPEND SERVER_ENV_EXTRA
            "RUSTFLAGS=${SERVER_AMBIENT_RUSTFLAGS}-Cprofile-generate=$ENV{SECANTUS_PGO_GENERATE}")
    elseif(EXISTS ${SERVER_PGO_GZ})
        # Extract the committed sparse profile at configure time (cmake -E tar
        # handles gzip cross-platform — no gzip/python dependency).
        file(MAKE_DIRECTORY ${SERVER_TARGET_DIR}/pgo)
        execute_process(
            COMMAND ${CMAKE_COMMAND} -E tar xzf ${SERVER_PGO_GZ}
            WORKING_DIRECTORY ${SERVER_TARGET_DIR}/pgo
            RESULT_VARIABLE _pgo_extract_rc)
        if(_pgo_extract_rc EQUAL 0 AND EXISTS ${SERVER_TARGET_DIR}/pgo/_secantus_server.profdata)
            message(STATUS "PGO: using committed profile for _secantus_server")
            list(APPEND SERVER_ENV_EXTRA
                "RUSTFLAGS=${SERVER_AMBIENT_RUSTFLAGS}-Cprofile-use=${SERVER_TARGET_DIR}/pgo/_secantus_server.profdata -Cllvm-args=-pgo-warn-missing-function")
        else()
            message(WARNING "PGO: could not extract ${SERVER_PGO_GZ}; building without PGO")
        endif()
    endif()

    # Same always-run custom-target shape as secantus_storage_ext above —
    # see the staleness note there.
    add_custom_target(secantus_server_ext ALL
        COMMAND ${CMAKE_COMMAND} -E env
            ${SERVER_ENV_EXTRA}
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

    # -------------------------------------------------------------------------
    # The standalone Rust server binary (R7), bundled into the wheel as the
    # `secantusd-rs` command. It statically links the SAME vendored WiredTiger
    # as the extensions above (via secantus-wt's build.rs). Installed into the
    # wheel's scripts dir, so a wheel built with this flag puts `secantusd-rs`
    # on PATH after `pip install` — deliberately distinct from the pure-Python
    # `secantusd-py` console script (`secantus.cli:main`, `[project.scripts]`),
    # which remains the default and is unaffected.
    #
    # Built on every platform: the bin links `static=wiredtiger` the same way the
    # extensions above do, which link WT successfully under MSVC too. The crate's
    # `[[bin]]` is named `secantusd-rs`, so cargo emits `secantusd-rs[.exe]`.
    # -------------------------------------------------------------------------
    set(SECANTUSDB_CRATE_DIR  ${CMAKE_CURRENT_SOURCE_DIR}/crates/secantusdb)
    set(SECANTUSDB_TARGET_DIR ${CARGO_TARGET_ROOT}/secantusdb-target)
    if(WIN32)
        set(SECANTUSDB_BIN_BUILT  ${SECANTUSDB_TARGET_DIR}/release/secantusd-rs.exe)
        set(SECANTUSDB_BIN_STAGED ${CMAKE_CURRENT_BINARY_DIR}/secantusd-rs.exe)
    else()
        set(SECANTUSDB_BIN_BUILT  ${SECANTUSDB_TARGET_DIR}/release/secantusd-rs)
        set(SECANTUSDB_BIN_STAGED ${CMAKE_CURRENT_BINARY_DIR}/secantusd-rs)
    endif()

    # Same always-run custom-target shape as the extensions above (cargo owns
    # freshness; copy_if_different keeps the no-change case cheap).
    add_custom_target(secantusdb_bin ALL
        COMMAND ${CMAKE_COMMAND} -E env
            SECANTUS_WT_INCLUDE=${WT_BINARY_DIR}/include
            SECANTUS_WT_LIB=${WT_BINARY_DIR}
            CARGO_TARGET_DIR=${SECANTUSDB_TARGET_DIR}
            ${CARGO_EXECUTABLE} build --release
                --manifest-path ${SECANTUSDB_CRATE_DIR}/Cargo.toml
        COMMAND ${CMAKE_COMMAND} -E copy_if_different ${SECANTUSDB_BIN_BUILT} ${SECANTUSDB_BIN_STAGED}
        DEPENDS wiredtiger_ext
        COMMENT "Building standalone secantusd-rs binary against bundled WiredTiger"
        VERBATIM
    )

    # Into the wheel's scripts dir so pip drops it in the env's bin/ (Scripts/ on
    # Windows) on PATH. PROGRAMS (not FILES) so the executable bit is preserved.
    install(PROGRAMS ${SECANTUSDB_BIN_STAGED} DESTINATION ${SKBUILD_SCRIPTS_DIR} OPTIONAL)
endif()
