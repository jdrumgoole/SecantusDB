# Stage a private copy of the vendored WiredTiger source into the build dir.
#
# The patch scripts REWRITE the files they touch. Applied to
# ``vendor/wiredtiger`` directly — which is what happened before this — that
# has two consequences:
#
#   1. Every build leaves the submodule dirty, so ``git status`` reports
#      modified vendor content forever after and a careless ``git add -A``
#      captures it.
#   2. It couples the patch state (in the source tree) to the ExternalProject
#      stamps (in the build dir). Those two can be restored independently — a
#      CI cache would hand a build dir whose stamps say "patched" to a freshly
#      checked-out, unpatched submodule — and anything that then recompiles
#      compiles UNPATCHED source. The patches cover musl ``off64_t``, strict
#      ``-Werror`` suppression and the Windows ``.pyd`` suffix, so that failure
#      is a subtly wrong artifact, not a red build.
#
# Building from a copy removes both: the checkout stays pristine, and patch
# state lives in the same directory as the stamps that describe it.
#
# Invoked as:
#   cmake -DSRC=... -DDST=... -DWT_SHA=... -P copy_wt_source.cmake
#
# ``WT_SHA`` is not read here. It is present so the vendored commit is part of
# ExternalProject's RECORDED download command: bumping the submodule changes
# the command text, which re-runs this copy and, because the later steps depend
# on it, re-runs the patch and configure steps too. Without it a stale copy
# would survive a WiredTiger bump.

foreach(var SRC DST)
    if(NOT DEFINED ${var})
        message(FATAL_ERROR "copy_wt_source.cmake: -D${var} is required")
    endif()
endforeach()

if(NOT EXISTS "${SRC}/CMakeLists.txt")
    message(FATAL_ERROR
        "copy_wt_source.cmake: ${SRC} does not look like the WiredTiger source "
        "(no CMakeLists.txt). Is the vendor/wiredtiger submodule checked out?")
endif()

# Remove first: copy_directory MERGES, so without this a file deleted upstream
# would linger in the copy and a re-copy after a bump would leave a hybrid tree.
file(REMOVE_RECURSE "${DST}")

execute_process(
    COMMAND ${CMAKE_COMMAND} -E copy_directory "${SRC}" "${DST}"
    RESULT_VARIABLE _rc
    ERROR_VARIABLE _err
)
if(NOT _rc EQUAL 0)
    message(FATAL_ERROR "copy_wt_source.cmake: copy failed (rc=${_rc}): ${_err}")
endif()

# Drop the submodule's git link from the copy. It is a ``.git`` FILE pointing
# back into the parent repo's modules dir, so leaving it makes the copy look
# like a checkout of the real submodule — and any git command run inside the
# build tree would then operate on the actual vendored repo.
file(REMOVE_RECURSE "${DST}/.git")

message(STATUS "staged WiredTiger source -> ${DST}")
