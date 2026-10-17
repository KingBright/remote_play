# Target-only CMake configuration. Never use the macOS archive indexer for ELF.
set(CMAKE_SYSTEM_NAME Linux)
set(CMAKE_SYSTEM_PROCESSOR x86_64)
set(CMAKE_C_COMPILER "${CMAKE_CURRENT_LIST_DIR}/linux-zig-cc")
set(CMAKE_CXX_COMPILER "${CMAKE_CURRENT_LIST_DIR}/linux-zig-cxx")
set(CMAKE_AR "${CMAKE_CURRENT_LIST_DIR}/linux-zig-ar" CACHE FILEPATH "Target archiver" FORCE)
set(CMAKE_RANLIB "${CMAKE_CURRENT_LIST_DIR}/linux-zig-ranlib" CACHE FILEPATH "Target archive indexer" FORCE)
# Cached CMakeCCompiler.cmake can also define normal variables that shadow the
# cache. The post-project hook must set both scopes for an existing build tree.
set(CMAKE_AR "${CMAKE_CURRENT_LIST_DIR}/linux-zig-ar")
set(CMAKE_RANLIB "${CMAKE_CURRENT_LIST_DIR}/linux-zig-ranlib")
if(DEFINED ENV{WEAVELO_VALIDATION_SYSROOT})
  set(CMAKE_SYSROOT "$ENV{WEAVELO_VALIDATION_SYSROOT}")
  set(CMAKE_FIND_ROOT_PATH "$ENV{WEAVELO_VALIDATION_SYSROOT}")
  set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
  set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
  set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)
  set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
endif()
