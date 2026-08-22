cmake_minimum_required(VERSION 3.24)
project(vim-design-cpp-test CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Where the Rust build put libvim_design (target/debug or target/release).
# devops/vbuild.ps1 passes this; default to the debug layout for manual use.
if(NOT DEFINED VIM_DESIGN_TARGET_DIR)
    set(VIM_DESIGN_TARGET_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../target/debug")
endif()
get_filename_component(VIM_DESIGN_TARGET_DIR "${VIM_DESIGN_TARGET_DIR}" ABSOLUTE)

set(VIM_DESIGN_INCLUDE_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../crates/vim-design-ffi/include")

find_library(VIM_DESIGN_LIB
    NAMES vim_design
    PATHS "${VIM_DESIGN_TARGET_DIR}"
    NO_DEFAULT_PATH
    REQUIRED)

# GoogleTest via FetchContent.
include(FetchContent)
FetchContent_Declare(
    googletest
    URL https://github.com/google/googletest/releases/download/v1.17.0/googletest-1.17.0.tar.gz
    DOWNLOAD_EXTRACT_TIMESTAMP TRUE
)
# For Windows: prevent overriding the parent project's compiler/linker settings.
set(gtest_force_shared_crt ON CACHE BOOL "" FORCE)
FetchContent_MakeAvailable(googletest)

enable_testing()

add_executable(vim_design_cpp_test test_ffi.cpp)
target_include_directories(vim_design_cpp_test PRIVATE "${VIM_DESIGN_INCLUDE_DIR}")
target_link_libraries(vim_design_cpp_test PRIVATE GTest::gtest_main "${VIM_DESIGN_LIB}")

# Find libvim_design.so next to the Rust target dir at run time.
set_target_properties(vim_design_cpp_test PROPERTIES
    BUILD_RPATH "${VIM_DESIGN_TARGET_DIR}"
    INSTALL_RPATH "${VIM_DESIGN_TARGET_DIR}")

include(GoogleTest)
gtest_discover_tests(vim_design_cpp_test)
