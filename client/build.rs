//! Build only the explicitly enabled Linux native ABI wrapper. Other platforms
//! and ordinary builds do not require FFmpeg headers or invoke a C compiler.
use std::{env, path::PathBuf};
mod build_toolchain;
fn main() {
    println!("cargo:rerun-if-env-changed=RP_VERIFIED_FFMPEG_INCLUDE");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux")
        || env::var_os("CARGO_FEATURE_NATIVE_LINUX_VIDEO").is_none()
    {
        return;
    }
    let headers = PathBuf::from(
        env::var_os("RP_VERIFIED_FFMPEG_INCLUDE")
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
