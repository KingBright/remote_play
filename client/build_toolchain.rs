//! Shared by the production build script and its isolated tool-selection tests.
use std::path::Path;

pub fn native_video(headers: &Path) -> cc::Build {
    let mut build = cc::Build::new();
    // Do not override compiler/archiver/target here. cc-rs reads Cargo's TARGET,
    // HOST and target-qualified CC/AR/CFLAGS without using the host tool by fiat.
    build
        .file("native/linux_video_bridge.c")
        .include(headers)
        .std("c11")
        .opt_level(2)
        .warnings(true)
        .extra_warnings(true)
        .warnings_into_errors(true)
        .pic(true);
    build
}
