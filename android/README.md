# Android build and verification

Use JDK 17, Android SDK 35 and an Android NDK. The checked-in Gradle 8.13 wrapper pins and verifies its distribution; use `./gradlew` instead of a global Gradle 9 installation. Plugin versions are in `settings.gradle.kts`.

From the repository root, build the JNI library before packaging the app:

```sh
rustup target add aarch64-linux-android x86_64-linux-android
./scripts/build_android_native.sh
cd android
./gradlew :app:assembleDebug :app:lintDebug
```

Set `JAVA_HOME`, `ANDROID_HOME` and, if necessary, `ANDROID_NDK_HOME` for your machine. `CARGO_TARGET_DIR` is supported by the native build script. Check its output: the fallback path skips x86_64 when that Rust target is absent. The APK needs a newly built `libremote_play_android.so` for every ABI being tested, especially after JNI interface changes. The Kotlin-only build does not regenerate native libraries.

The verified macOS build used HotSpot JDK 17 and these Gradle resource limits:

```sh
./gradlew --no-daemon --max-workers=4 \
  -Dorg.gradle.jvmargs='-Xmx2g -XX:MaxMetaspaceSize=1g' \
  :app:assembleDebug :app:lintDebug
```

Connect using a discovered host or enter `IP:port`; the default host port is 39271. An Android emulator reaches its development host at `10.0.2.2:39271`. `127.0.0.1` refers to the Android device itself. The current native bridge uses UDP and HEVC. HEVC initialization failure is displayed explicitly; AVC requires sender-side codec negotiation and is not an automatic decoder fallback.

`Decoded N FPS` counts codec output buffers released to the Surface during a monotonic 500 ms sampling window. It does not measure display scanout or end-to-end latency. Unsupported file, microphone and clipboard controls are marked unavailable, and unmeasured channel rates are not shown as zero.

See `docs/reviews/2026-09-12/FOLLOWUP.md` in the repository root for verification evidence and remaining platform limits.
