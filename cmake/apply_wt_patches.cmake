# Driver for the vendored-WiredTiger source patches.
#
# Exists for one reason: to keep a VOLATILE path out of ExternalProject's
# recorded PATCH_COMMAND.
#
# ExternalProject writes the literal patch command into
# ``wiredtiger_ext-stamp/wiredtiger_ext-patch-info.txt`` and re-runs the patch
# step whenever that text changes. The command used to start with
# ``${Python3_EXECUTABLE}``, which under a PEP 517 build is the *isolated build
# environment's* interpreter — e.g.
# ``~/.cache/uv/builds-v0/.tmpkFPFKt/bin/python`` — and that temp directory is
# different on every single build. So the recorded command changed every time,
# the patch stamp was invalidated every time, and configure + build cascaded
# behind it: WiredTiger recompiled from scratch on every build even though
# ``BUILD_ALWAYS OFF`` is set and nothing had actually changed.
#
# The interpreter is therefore passed by FILE rather than as an argument: the
# command below contains only stable paths, while the file's contents may vary
# freely. That is sound because the patches are idempotent text edits on the
# vendored source — which interpreter applies them has no bearing on the result,
# so a changed interpreter is not a reason to re-patch or rebuild.
#
# Invoked as:
#   cmake -DPY_FILE=... -DSCRIPT_DIR=... -DWT_SOURCE_DIR=... -P apply_wt_patches.cmake

foreach(var PY_FILE SCRIPT_DIR WT_SOURCE_DIR)
    if(NOT DEFINED ${var})
        message(FATAL_ERROR "apply_wt_patches.cmake: -D${var} is required")
    endif()
endforeach()

if(NOT EXISTS "${PY_FILE}")
    message(FATAL_ERROR
        "apply_wt_patches.cmake: interpreter file not found: ${PY_FILE}\n"
        "It is written at configure time by the top-level CMakeLists.txt.")
endif()

file(READ "${PY_FILE}" WT_PATCH_PYTHON)
string(STRIP "${WT_PATCH_PYTHON}" WT_PATCH_PYTHON)

if(WT_PATCH_PYTHON STREQUAL "" OR NOT EXISTS "${WT_PATCH_PYTHON}")
    message(FATAL_ERROR
        "apply_wt_patches.cmake: interpreter '${WT_PATCH_PYTHON}' (from "
        "${PY_FILE}) does not exist.")
endif()

# script;target pairs — same set, same order as the previous inline
# PATCH_COMMAND chain.
set(_patches
    "patch_wt_strict.py|${WT_SOURCE_DIR}/cmake/strict/strict_flags_helpers.cmake"
    "patch_wt_python.py|${WT_SOURCE_DIR}/lang/python/CMakeLists.txt"
    "patch_wt_helpers.py|${WT_SOURCE_DIR}/cmake/helpers.cmake"
    "patch_wt_musl.py|${WT_SOURCE_DIR}/src/os_posix/os_fs.c"
)

foreach(_entry IN LISTS _patches)
    string(REPLACE "|" ";" _parts "${_entry}")
    list(GET _parts 0 _script)
    list(GET _parts 1 _target)
    execute_process(
        COMMAND "${WT_PATCH_PYTHON}" "${SCRIPT_DIR}/${_script}" "${_target}"
        RESULT_VARIABLE _rc
        OUTPUT_VARIABLE _out
        ERROR_VARIABLE _err
    )
    if(NOT _rc EQUAL 0)
        # Fail loudly: a silently unpatched WT source builds, then breaks in
        # ways (musl off64_t, missing includes, no .pyd suffix) that are far
        # harder to trace back here.
        message(FATAL_ERROR
            "apply_wt_patches.cmake: ${_script} failed (rc=${_rc})\n"
            "target: ${_target}\nstdout: ${_out}\nstderr: ${_err}")
    endif()
    if(_out)
        message(STATUS "${_script}: ${_out}")
    endif()
endforeach()
