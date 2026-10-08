# Fixed GPUI native video extension

GPUI CE remains version 0.3.3. `gpui-ce.UPSTREAM.json` records the original registry archive SHA-256 and file hashes. The workspace patch points only this package to `third_party/gpui-ce`; no dependency-version upgrade is implied. Original licenses remain in the vendor directory.

The `native-video` feature is disabled by default. The application enables it explicitly with `--features gpui-native-video`. Do not switch the installed app or its default entry before decoder integration, Linux native import, preserved-feature acceptance and real performance validation.

Development validation uses `cargo run -p remote_play_app --example native_video_gpu_check --features gpui-native-video` on a physical Windows D3D11 adapter, with `RP_NATIVE_VIDEO_GPU_TEST=1`. It generates immutable test textures and reads back only a 128x96 offscreen validation target. The compositor itself maps only its 144-byte parameter buffer, never video pixels. `native_video_original_window` adds an actual GPUI window and the original capsule component; its receipt distinguishes submitted/GPU-completed work from scan-out.

No tests initialize RemotePlay networking, screen capture, microphone, file sharing or OS input injection. No software-GPU fallback is allowed in the GPU check. A successful synthetic pattern does not prove actual decoder-to-screen zero-copy, remote sessions or peak performance.
