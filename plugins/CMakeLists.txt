cmake_minimum_required(VERSION 3.16)
project(waywallen-plugins
    VERSION 0.1.0
    LANGUAGES C CXX
    DESCRIPTION "waywallen renderer subprocess plugins (image, video, mpv)"
)

# Toggle which plugins are built. image+video default ON (their FFmpeg/Vulkan
# deps are part of the standard build env); mpv defaults OFF because libmpv +
# EGL + GBM aren't in environment.yml — opt in only if you have them locally.
option(WAYWALLEN_BUILD_IMAGE_PLUGIN "Build the ffmpeg+Vulkan image plugin" ON)
option(WAYWALLEN_BUILD_VIDEO_PLUGIN "Build the ffmpeg+Vulkan video plugin" ON)
option(WAYWALLEN_BUILD_MPV_PLUGIN   "Build the libmpv+EGL/GBM video plugin" OFF)

# Resolve waywallen-bridge once so every plugin in this tree shares the same
# target instead of rebuilding it per-subdir. Plugins that also stand alone
# guard their own bridge block with `if(NOT TARGET waywallen::bridge)`.
if(NOT TARGET waywallen::bridge)
    add_subdirectory(../bridge waywallen-bridge-build)
endif()

# Same idea for waywallen::ffmpeg_vk — Iter 1 hoisted the per-plugin
# VkProducer into libs/ffmpeg so image + video plugins share one copy.
# Pulling the subdir in here means rstd-less plugin builds work fine
# (libs/ffmpeg gates the rstd-dependent thumbnail target on rstd).
if(NOT TARGET waywallen::ffmpeg_vk)
    add_subdirectory(../libs/ffmpeg waywallen-ffmpeg-build)
endif()

if(WAYWALLEN_BUILD_IMAGE_PLUGIN)
    add_subdirectory(image)
endif()

if(WAYWALLEN_BUILD_VIDEO_PLUGIN)
    add_subdirectory(video)
endif()

if(WAYWALLEN_BUILD_MPV_PLUGIN)
    add_subdirectory(mpv)
endif()
