cmake_minimum_required(VERSION 3.20)
project(kokoro_sapi LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# x86 SAPI5 engine for 32-bit hosts (Kindle.exe loads it in-process via
# ISpVoice). It is connect-only: all synthesis happens in the kokoro-kindle-reader app
# (kokoro-js on WebGPU), reached over a named pipe (see src/WorkerProtocol.h and
# pipe_server.rs in the Tauri app). The engine links no ONNX/espeak/DirectML —
# it's a thin COM shim + pipe client.

add_library(kokoro_sapi SHARED
  src/Guids.cpp
  src/KokoroTTSEngine.cpp
  src/WorkerClient.cpp
  src/Dll.cpp
  src/kokoro_sapi.def
)
set_target_properties(kokoro_sapi PROPERTIES OUTPUT_NAME "KokoroSapi")
target_compile_definitions(kokoro_sapi PRIVATE UNICODE _UNICODE)
# ole32 -> StringFromGUID2 / CoTaskMemAlloc;  advapi32 -> registry self-registration.
target_link_libraries(kokoro_sapi PRIVATE ole32 advapi32)

# Diagnostic: enumerate the SAPI voices a host can see.
add_executable(onecore_probe tools/onecore_probe.cpp)
target_compile_definitions(onecore_probe PRIVATE UNICODE _UNICODE)
target_link_libraries(onecore_probe PRIVATE ole32 sapi)
