# Linux native video development boundary

This directory is not yet part of the shipping playback implementation. It contains a small C ABI wrapper around the installed FFmpeg 8.x public API and a separate Rust/Vulkan validation program. The C layer retains native AVFrames and DRM objects only; Rust remains responsible for product scheduling, session generations, bounded queues, input safety and presentation.

## Verified scope

On HO5, the wrapper decoded HEVC using VAAPI and exported hardware frames as DRM PRIME using READ|DIRECT mapping. No software decode fallback or full-frame CPU output is requested. Mapping synchronizes the decoder in the native worker, not a GUI/network task. Exported FDs remain owned by the mapped native frame until its consumer releases it.

The standalone `vulkan_probe` imports the actual format/modifier/plane offset/pitch into the exact physical render-node GPU, verifies import capability and memory compatibility, transfers ownership between foreign and Vulkan queues, and checks small samples against a generated fixture's software reference. It uses a private Vulkan context, **not** GPUI's active Blade context. Its successful result is not a product screen-presentation receipt.

Tests passed for generated 8-bit and 10-bit HEVC, with three repeats each. Every run decoded/direct-mapped 60 frames; only the first frame was imported for GPU pixel verification, at top-left, center and bottom-right of each plane. GPU-accepted formats and driver-specific tiling must not be inferred from another machine's result.

## Build provenance and packaging

The validation was built against the official signed FFmpeg 8.1.2 headers matching HO5's installed `libavcodec.so.62` and `libavutil.so.60`. The public source archive SHA-256 is `464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c`; the signing fingerprint verified was `FCF986EA15E6E293A5644F10B4322F04D67658D8`. Headers were configured only in a private staging directory; no system package or graphics driver was installed or replaced.

`RP_VERIFIED_FFMPEG_INCLUDE` is mandatory for the validation build. ABI layout size and runtime library major versions are checked. Its own Cargo.lock pins the small validation dependency graph; the product Cargo.lock has not changed. Do not ship the machine's entire FFmpeg build or assume this development setup is a portable redistribution package. Release packaging and license/capability verification remain required.

## Integration gate

The current Blade 0.7.1 public texture API does not express explicit DRM modifier plane layouts or foreign ownership transitions. Do not pass this decoder's tiled DMA-BUF through its ordinary optimal-tiling texture constructor, use a CPU image workaround, transmute private renderer structs, or silently switch rendering engines. The confirmed native import can inform a narrowly scoped, pinned renderer interop extension, but adding that dependency-level patch has not been adopted in this iteration.

Before product activation, implement the native frame owner/generation path, bounded imported-resource cache, GPU completion retirement, original GPUI scene masks/overlays and input mapping. The readback in the validation program is test-only (144 or 288 selected bytes), not a shipping playback stage. No latency/FPS/zero-copy or long-duration performance claim follows from these tests.
