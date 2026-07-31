# get_cpm.cmake — vendored into your project by `cpm init`.
# Bootstraps the official CPM.cmake (downloaded once into CPM_SOURCE_CACHE).
set(CPM_DOWNLOAD_VERSION 0.43.1)
set(CPM_HASH_SUM "1c40fc102ce9625d7de7eb14f541cab30cc3138dca627f0b0ec40293ce6c2934")

if(CPM_SOURCE_CACHE)
  set(CPM_DOWNLOAD_LOCATION "${CPM_SOURCE_CACHE}/cpm/CPM_${CPM_DOWNLOAD_VERSION}.cmake")
elseif(DEFINED ENV{CPM_SOURCE_CACHE})
  set(CPM_DOWNLOAD_LOCATION "$ENV{CPM_SOURCE_CACHE}/cpm/CPM_${CPM_DOWNLOAD_VERSION}.cmake")
else()
  set(CPM_DOWNLOAD_LOCATION "${CMAKE_BINARY_DIR}/cmake/CPM_${CPM_DOWNLOAD_VERSION}.cmake")
endif()

get_filename_component(CPM_DOWNLOAD_LOCATION "${CPM_DOWNLOAD_LOCATION}" ABSOLUTE)

if(NOT EXISTS "${CPM_DOWNLOAD_LOCATION}")
  message(STATUS "CPM: downloading CPM.cmake v${CPM_DOWNLOAD_VERSION} ...")
  file(DOWNLOAD
       "https://github.com/cpm-cmake/CPM.cmake/releases/download/v${CPM_DOWNLOAD_VERSION}/CPM.cmake"
       "${CPM_DOWNLOAD_LOCATION}"
       EXPECTED_HASH SHA256=${CPM_HASH_SUM}
       STATUS _cpm_dl)
  list(GET _cpm_dl 0 _cpm_code)
  if(NOT _cpm_code EQUAL 0)
    message(FATAL_ERROR "CPM: failed to download CPM.cmake (status: ${_cpm_dl}).\n"
                        "Run online once, or place CPM_${CPM_DOWNLOAD_VERSION}.cmake at\n"
                        "  ${CPM_DOWNLOAD_LOCATION}")
  endif()
endif()

include("${CPM_DOWNLOAD_LOCATION}")
