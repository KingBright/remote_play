fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Host test/example executables also link ScreenCaptureKit's Swift bridge.
    // Dependency build-script link arguments do not set their runtime rpath.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
