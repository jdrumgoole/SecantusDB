# Records the absl string hashes and flat_hash_map orders that
# crates/secantus-core/tests/absl_hash_order.rs checks the port against.
#
#   git clone --depth 1 --branch 20250512.1-mongo-1 \
#       https://github.com/mongodb-forks/abseil-cpp.git .scratch/abseil
#   cmake -S tools/probes/absl_order -B .scratch/absl-x86 \
#       -DABSL_DIR=$PWD/.scratch/abseil -DCMAKE_OSX_ARCHITECTURES=x86_64
#   cmake --build .scratch/absl-x86 --target harness
#   arch -x86_64 .scratch/absl-x86/harness > .scratch/w16.json   # SSE2, width 16
#
# (arm64, NEON: the same with arm64 and no `arch`, width 8.) The fixtures are
# the JSON reformatted as text: see the header of each fixture file.
cmake_minimum_required(VERSION 3.16)
project(abslorder CXX)
set(CMAKE_CXX_STANDARD 17)
set(ABSL_PROPAGATE_CXX_STD ON)
set(CMAKE_BUILD_TYPE Release)
add_subdirectory(${ABSL_DIR} abseil EXCLUDE_FROM_ALL)
add_executable(harness harness.cc)
target_link_libraries(harness absl::flat_hash_map absl::hash)
