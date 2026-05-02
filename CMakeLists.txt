cmake_minimum_required(VERSION 3.28)
project(waywallen
    VERSION 0.1.0
    LANGUAGES C CXX
    DESCRIPTION "waywallen: Rust daemon + Qt/QML UI + renderer subprocess plugins"
)

if(POLICY CMP0077)
    cmake_policy(SET CMP0077 NEW)
endif()

include(GNUInstallDirs)
list(PREPEND CMAKE_MODULE_PATH "${CMAKE_CURRENT_SOURCE_DIR}/cmake")

option(WAYWALLEN_BUILD_DAEMON  "Build the Rust daemon via Corrosion"   ON)
option(WAYWALLEN_BUILD_UI      "Build the Qt/QML UI"                   ON)
option(WAYWALLEN_BUILD_PLUGINS "Build renderer plugins (image, video)" ON)

if(WAYWALLEN_BUILD_DAEMON)
    include(FetchCorrosion)
    # NOTE: no LOCKED here — Cargo.lock is gitignored in this repo, so cargo
    # must be allowed to (re)generate it on a fresh checkout. Flip to FROZEN
    # if a committed Cargo.lock is added later for reproducible builds.
    corrosion_import_crate(
        MANIFEST_PATH "${CMAKE_CURRENT_SOURCE_DIR}/Cargo.toml"
        CRATES        waywallen waywallen-display-layer-shell
    )
    install(IMPORTED_RUNTIME_ARTIFACTS
        waywallen
        waywallen-display-layer-shell
        RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR}
        COMPONENT Daemon
    )
endif()

if(WAYWALLEN_BUILD_PLUGINS)
    add_subdirectory(plugins)
endif()

if(WAYWALLEN_BUILD_UI)
    add_subdirectory(ui)
endif()

message(STATUS
    "waywallen components: daemon=${WAYWALLEN_BUILD_DAEMON} ui=${WAYWALLEN_BUILD_UI} plugins=${WAYWALLEN_BUILD_PLUGINS}")

include(CPackConfig)
