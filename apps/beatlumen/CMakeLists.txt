cmake_minimum_required(VERSION 3.21)

project(beatlumen VERSION 0.1.0 LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

find_package(Qt6 6.5 REQUIRED COMPONENTS Core Gui Qml Quick QuickControls2 Svg OpenGL Network)
find_package(Qt6 6.5 QUIET COMPONENTS Multimedia)

qt_standard_project_setup(REQUIRES 6.5)

# ---- Rust core (crates/bl-ffi) ----------------------------------------------
set(BL_WORKSPACE "${CMAKE_CURRENT_SOURCE_DIR}/../..")
set(BL_CARGO "cargo" CACHE STRING "cargo executable")
set(BL_RUST_TARGET "" CACHE STRING "Rust target triple (empty: derived from the platform)")
if(NOT BL_RUST_TARGET)
    if(ANDROID)
        if(ANDROID_ABI STREQUAL "arm64-v8a")
            set(BL_RUST_TARGET aarch64-linux-android)
        elseif(ANDROID_ABI STREQUAL "armeabi-v7a")
            set(BL_RUST_TARGET armv7-linux-androideabi)
        elseif(ANDROID_ABI STREQUAL "x86_64")
            set(BL_RUST_TARGET x86_64-linux-android)
        else()
            set(BL_RUST_TARGET i686-linux-android)
        endif()
    elseif(EMSCRIPTEN)
        set(BL_RUST_TARGET wasm32-unknown-emscripten)
    endif()
endif()
set(BL_CARGO_ENV "")
if(EMSCRIPTEN)
    # Qt for WebAssembly uses JavaScript exceptions; Rust panics must not unwind.
    set(BL_CARGO_ENV "CARGO_TARGET_WASM32_UNKNOWN_EMSCRIPTEN_RUSTFLAGS=-C panic=abort")
endif()
if(CMAKE_BUILD_TYPE STREQUAL "Debug")
    set(BL_CARGO_PROFILE dev)
    set(BL_CARGO_DIR debug)
else()
    set(BL_CARGO_PROFILE release)
    set(BL_CARGO_DIR release)
endif()
set(BL_CARGO_TARGET_DIR "${CMAKE_CURRENT_BINARY_DIR}/cargo")
if(BL_RUST_TARGET)
    set(BL_CARGO_TARGET_ARGS --target ${BL_RUST_TARGET})
    set(BL_CORE_LIB_DIR "${BL_CARGO_TARGET_DIR}/${BL_RUST_TARGET}/${BL_CARGO_DIR}")
else()
    set(BL_CARGO_TARGET_ARGS "")
    set(BL_CORE_LIB_DIR "${BL_CARGO_TARGET_DIR}/${BL_CARGO_DIR}")
endif()
if(MSVC)
    set(BL_CORE_LIB "${BL_CORE_LIB_DIR}/beatlumen_core.lib")
else()
    set(BL_CORE_LIB "${BL_CORE_LIB_DIR}/libbeatlumen_core.a")
endif()
file(GLOB_RECURSE BL_RUST_SOURCES CONFIGURE_DEPENDS "${BL_WORKSPACE}/crates/*.rs" "${BL_WORKSPACE}/crates/*.toml" "${BL_WORKSPACE}/crates/*.hfx")
add_custom_command(
    OUTPUT "${BL_CORE_LIB}"
    COMMAND ${CMAKE_COMMAND} -E env CARGO_TARGET_DIR=${BL_CARGO_TARGET_DIR} ${BL_CARGO_ENV}
            ${BL_CARGO} build --profile ${BL_CARGO_PROFILE} -p bl-ffi ${BL_CARGO_TARGET_ARGS}
    WORKING_DIRECTORY "${BL_WORKSPACE}"
    DEPENDS ${BL_RUST_SOURCES}
    COMMENT "Building the Rust core"
    USES_TERMINAL
    VERBATIM)
add_custom_target(beatlumen_core_build DEPENDS "${BL_CORE_LIB}")
add_library(beatlumen_core STATIC IMPORTED GLOBAL)
set_target_properties(beatlumen_core PROPERTIES IMPORTED_LOCATION "${BL_CORE_LIB}")
add_dependencies(beatlumen_core beatlumen_core_build)

# ---- application ----------------------------------------------------------------
qt_add_executable(beatlumen
    src/main.cpp
    src/bridge.cpp src/bridge.h
    src/audio.cpp src/audio.h
    src/web.cpp src/web.h
    src/testhooks.cpp src/testhooks.h
    src/tls.cpp src/tls.h
    src/core.h
)
target_compile_definitions(beatlumen PRIVATE BL_VERSION="${PROJECT_VERSION}")

file(GLOB BL_QML RELATIVE "${CMAKE_CURRENT_SOURCE_DIR}" CONFIGURE_DEPENDS qml/*.qml)
# File pickers: Qt Quick Dialogs on desktop; the system document picker on
# Android and in the browser (keeps the other Qt Quick Controls styles out).
# BL_MOBILE_DIALOGS=ON uses the mobile variant on desktop (for testing).
option(BL_MOBILE_DIALOGS "Use the mobile file pickers on desktop" OFF)
if(ANDROID OR EMSCRIPTEN OR BL_MOBILE_DIALOGS)
    list(APPEND BL_QML platform/mobile/PlatformDialogs.qml)
else()
    # Kept as .qml.in so that the import scanner of mobile builds, which scans
    # the whole source directory, does not see the Qt Quick Dialogs import.
    configure_file(platform/desktop/PlatformDialogs.qml.in "${CMAKE_CURRENT_BINARY_DIR}/qml/PlatformDialogs.qml" COPYONLY)
    list(APPEND BL_QML "${CMAKE_CURRENT_BINARY_DIR}/qml/PlatformDialogs.qml")
endif()
file(GLOB BL_ICONS RELATIVE "${CMAKE_CURRENT_SOURCE_DIR}" CONFIGURE_DEPENDS icons/*.svg)
set_source_files_properties(qml/Theme.qml qml/Covers.qml PROPERTIES QT_QML_SINGLETON_TYPE TRUE)
# Place the QML files at the module root so that they see each other
# (including the singletons) through the module's qmldir.
foreach(f ${BL_QML})
    get_filename_component(n "${f}" NAME)
    set_source_files_properties(${f} PROPERTIES QT_RESOURCE_ALIAS ${n})
endforeach()
# Only scan the QML that is actually built for imports to deploy.
if(ANDROID OR EMSCRIPTEN)
    set_target_properties(beatlumen PROPERTIES QT_QML_ROOT_PATH "${CMAKE_CURRENT_SOURCE_DIR}/qml;${CMAKE_CURRENT_SOURCE_DIR}/platform/mobile")
endif()
qt_add_qml_module(beatlumen
    URI Beatlumen
    VERSION 1.0
    QML_FILES ${BL_QML}
    RESOURCES ${BL_ICONS} assets/Inter-Variable.ttf assets/icon.png
)

target_link_libraries(beatlumen PRIVATE
    Qt6::Core Qt6::Gui Qt6::Qml Qt6::Quick Qt6::QuickControls2 Qt6::Svg Qt6::OpenGL Qt6::Network
    beatlumen_core)
# Android plays through AAudio directly (see audio.cpp).
if(TARGET Qt6::Multimedia AND NOT EMSCRIPTEN AND NOT ANDROID)
    target_link_libraries(beatlumen PRIVATE Qt6::Multimedia)
    target_compile_definitions(beatlumen PRIVATE BL_HAVE_MULTIMEDIA)
endif()

# "Stay unlocked on this device": the platform's protected store.
target_sources(beatlumen PRIVATE src/secrets.cpp src/secrets.h)
# The desktop's media controls and media keys (media_desktop.cpp).
if(WIN32)
    target_sources(beatlumen PRIVATE src/media_desktop.cpp src/media_desktop.h)
    target_compile_definitions(beatlumen PRIVATE BEATLUMEN_SMTC)
    target_link_libraries(beatlumen PRIVATE runtimeobject shcore crypt32)
elseif(UNIX AND NOT APPLE AND NOT ANDROID AND NOT EMSCRIPTEN)
    find_package(Qt6 QUIET COMPONENTS DBus)
    if(TARGET Qt6::DBus)
        target_sources(beatlumen PRIVATE src/media_desktop.cpp src/media_desktop.h)
        target_compile_definitions(beatlumen PRIVATE BEATLUMEN_MPRIS)
        target_link_libraries(beatlumen PRIVATE Qt6::DBus)
    endif()
endif()

if(WIN32)
    # Icon and version information of beatlumen.exe.
    configure_file(windows/beatlumen.rc.in "${CMAKE_CURRENT_BINARY_DIR}/beatlumen.rc" @ONLY)
    target_sources(beatlumen PRIVATE "${CMAKE_CURRENT_BINARY_DIR}/beatlumen.rc")
    target_link_libraries(beatlumen PRIVATE ws2_32 userenv bcrypt ntdll advapi32 synchronization)
    set_target_properties(beatlumen PROPERTIES WIN32_EXECUTABLE ON)
elseif(ANDROID)
    find_package(Qt6 REQUIRED COMPONENTS CorePrivate)
    target_link_libraries(beatlumen PRIVATE Qt6::CorePrivate log android aaudio)
    # Background playback (MediaSession, foreground service): the Java side
    # lives in android/src.
    target_sources(beatlumen PRIVATE src/android_media.cpp src/android_media.h src/android_capture.cpp src/android_capture.h)
    # Deploy only the plugins the app uses: no Qt Multimedia (audio goes
    # through AAudio), no QML debugging, TLS or network information.
    qt_import_plugins(beatlumen
        INCLUDE_BY_TYPE
            imageformats Qt6::QJpegPlugin Qt6::QWebpPlugin Qt6::QGifPlugin Qt6::QSvgPlugin
        EXCLUDE Qt6::QOffscreenIntegrationPlugin
        EXCLUDE_BY_TYPE qmltooling tls networkinformation multimedia)
    set(BL_ANDROID_VERSION_CODE 3 CACHE STRING "android:versionCode")
    # Compressed native libraries: the APK is sideloaded, so its download
    # size matters more than the extraction at install time.
    option(BL_ANDROID_COMPRESS_LIBS "Store the native libraries compressed in the APK" ON)
    set_target_properties(beatlumen PROPERTIES
        QT_ANDROID_PACKAGE_SOURCE_DIR "${CMAKE_CURRENT_SOURCE_DIR}/android"
        QT_ANDROID_PACKAGE_NAME "io.github.randomnam3.beatlumen"
        QT_ANDROID_APP_NAME "Beatlumen"
        QT_ANDROID_TARGET_SDK_VERSION 35
        QT_ANDROID_MIN_SDK_VERSION 26
        QT_ANDROID_VERSION_NAME "${PROJECT_VERSION}"
        QT_ANDROID_VERSION_CODE ${BL_ANDROID_VERSION_CODE}
        QT_ANDROID_LEGACY_PACKAGING ${BL_ANDROID_COMPRESS_LIBS})
elseif(EMSCRIPTEN)
    enable_language(ASM)
    target_sources(beatlumen PRIVATE src/wasm_eh_tag.s)
    set_source_files_properties(src/wasm_eh_tag.s PROPERTIES LANGUAGE ASM COMPILE_OPTIONS "-mexception-handling")
    # No eval: the server sends a Content-Security-Policy without unsafe-eval.
    target_link_options(beatlumen PRIVATE -sDYNAMIC_EXECUTION=0 -sGL_ENABLE_GET_PROC_ADDRESS=1 -sALLOW_MEMORY_GROWTH=1 -lidbfs.js
        "-sEXPORTED_FUNCTIONS=_main,_malloc,_free,_bl_web_storage_ready,_bl_web_file_picked"
        "-sEXPORTED_RUNTIME_METHODS=UTF8ToString,stringToNewUTF8,HEAPU8,HEAPF32")
else()
    find_package(Threads REQUIRED)
    target_link_libraries(beatlumen PRIVATE Threads::Threads ${CMAKE_DL_LIBS} m)
endif()

install(TARGETS beatlumen RUNTIME DESTINATION bin BUNDLE DESTINATION . LIBRARY DESTINATION lib)
