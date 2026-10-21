//! Platform ABI bridges: macOS observation and opt-in Linux native video.
//! FFmpeg headers are required only for native-linux-video.
use std::{env, path::PathBuf};
mod build_toolchain;
fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new().file("native/macos_observation.m")
            .flag("-fobjc-arc").flag("-fblocks").compile("weavelo_observation");
        for framework in ["Foundation", "CoreImage", "CoreGraphics", "CoreVideo"] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
        println!("cargo:rerun-if-changed=native/macos_observation.m");
    }
    println!("cargo:rerun-if-env-changed=WEAVELO_VERIFIED_FFMPEG_INCLUDE");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux")
        || env::var_os("CARGO_FEATURE_NATIVE_LINUX_VIDEO").is_none()
    {
        return;
    }
    let headers = PathBuf::from(
        env::var_os("WEAVELO_VERIFIED_FFMPEG_INCLUDE")
            .expect("native-linux-video needs explicitly verified FFmpeg 8.x development headers"),
    );
    assert!(headers.join("libavcodec/avcodec.h").is_file());
    build_toolchain::native_video(&headers).compile("rp_linux_native");
    // Versioned link targets avoid accidentally using a different installed ABI.
    // GNU link names are recorded in metadata, so dependent executables link them.
    println!("cargo:rustc-link-lib=dylib:+verbatim=libavcodec.so.62");
    println!("cargo:rustc-link-lib=dylib:+verbatim=libavutil.so.60");
    println!("cargo:rerun-if-changed=build_toolchain.rs");
    println!("cargo:rerun-if-changed=native/linux_video_bridge.c");
    println!("cargo:rerun-if-changed=native/linux_video_bridge.h");
}
