//! Development-only generated-texture check; never starts the RemotePlay runtime.
#[cfg(target_os="windows")]
fn main() {
    if let Err(error)=gpui::native_video::validate_windows_gpu() {
        eprintln!("NATIVE_GPU_VALIDATION_FAILED {error:#}");
        std::process::exit(1);
    }
    println!("NATIVE_GPU_VALIDATION_PASSED capture=false network=false input=false");
}
#[cfg(not(target_os="windows"))]
fn main() {
    eprintln!("This GPU validation uses D3D11; no substitute software test was run.");
    std::process::exit(2);
}
