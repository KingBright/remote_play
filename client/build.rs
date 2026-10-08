//! Build only the explicitly enabled Linux native ABI wrapper. Other platforms
//! and ordinary builds do not require FFmpeg headers or invoke a C compiler.
use std::{env, path::PathBuf, process::Command};
fn run(command: &mut Command) {
    let status = command
        .status()
        .expect("native-video build tool unavailable");
    assert!(
        status.success(),
        "native-video ABI wrapper build failed: {command:?}"
    );
}
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
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    run(Command::new("cc")
        .args([
            "-std=c11",
            "-O2",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-fPIC",
            "-c",
            "native/linux_video_bridge.c",
            "-I",
        ])
        .arg(headers)
        .arg("-o")
        .arg(out.join("native_video.o")));
    run(Command::new("ar")
        .arg("crs")
        .arg(out.join("librp_linux_native.a"))
        .arg(out.join("native_video.o")));
    // Versioned link targets avoid accidentally using a different installed ABI.
    // GNU link names are recorded in metadata, so dependent executables link them.
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=rp_linux_native");
    println!("cargo:rustc-link-lib=dylib:+verbatim=libavcodec.so.62");
    println!("cargo:rustc-link-lib=dylib:+verbatim=libavutil.so.60");
    println!("cargo:rerun-if-changed=native/linux_video_bridge.c");
    println!("cargo:rerun-if-changed=native/linux_video_bridge.h");
}
