use std::{env,path::PathBuf,process::Command};
fn run(c:&mut Command){let s=c.status().expect("build tool missing");assert!(s.success(),"native validation build command failed: {c:?}");}
fn main(){
    assert_eq!(env::var("CARGO_CFG_TARGET_OS").unwrap(),"linux","Linux-only explicit native validation");
    let include=PathBuf::from(env::var_os("RP_VERIFIED_FFMPEG_INCLUDE").expect("explicit verified matching FFmpeg headers required"));
    assert!(include.join("libavcodec/avcodec.h").is_file());
    let out=PathBuf::from(env::var_os("OUT_DIR").unwrap());
    run(Command::new("cc").args(["-std=c11","-O2","-Wall","-Wextra","-Werror","-fPIC","-c","../linux_video_bridge.c","-I"]).arg(include).arg("-o").arg(out.join("bridge.o")));
    run(Command::new("ar").arg("crs").arg(out.join("librp_video_bridge.a")).arg(out.join("bridge.o")));
    println!("cargo:rustc-link-search=native={}",out.display());
    println!("cargo:rustc-link-lib=static=rp_video_bridge");
    println!("cargo:rustc-link-arg=-Wl,-l:libavcodec.so.62");
    println!("cargo:rustc-link-arg=-Wl,-l:libavutil.so.60");
    println!("cargo:rerun-if-changed=../linux_video_bridge.c");
    println!("cargo:rerun-if-changed=../linux_video_bridge.h");
    println!("cargo:rerun-if-env-changed=RP_VERIFIED_FFMPEG_INCLUDE");
}
