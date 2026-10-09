# DesignCraft for Android

The Gradle project that packages `apps/designcraft-android` (the Rust app as a `GameActivity`
shell) into an APK/AAB. CI (`.github/workflows/android.yml`) builds it on every push to the
`android` branch and attaches signed builds to a GitHub Release on `android-v*` tags.

## How it fits together

- `apps/designcraft-android/src/lib.rs`: `android_main`, the platform services (open, save,
  preferences, URLs) and the JNI bridge to `MainActivity`. The services mirror the web shell
  (`apps/designcraft-web/src/web.rs`): picked files arrive through `Services::inbox`.
- `app/src/main/java/.../MainActivity.kt`: the Storage Access Framework picker, saving into
  `Downloads/DesignCraft/`, Help links, and the full-screen window.
- `cargo ndk` drops `libdesigncraft_android.so` into `app/src/main/jniLibs/arm64-v8a/` (ignored by
  git); Gradle packages it.

## Building locally

Needs the Android SDK (platform 35, build-tools 35), NDK r27, a stable Rust toolchain with the
`aarch64-linux-android` target, and `cargo-ndk`:

```sh
rustup target add aarch64-linux-android
cargo install cargo-ndk
export ANDROID_NDK_HOME=$ANDROID_SDK_ROOT/ndk/<version>
cargo ndk -t arm64-v8a --platform 30 -o android/app/src/main/jniLibs build --release -p designcraft-android
cd android && ./gradlew assembleDebug
```

Set `CRAFT_FONTS_DIR` to a checkout of storytold/craft-fonts to embed the shared fonts (CI does).

## Known limits (first version)

- Save and Export write to `Downloads/DesignCraft/<name>` without a dialog (like the web build:
  Save always downloads the document); saving the same name again in one session overwrites it.
- Open, Place and Load Swatches go through the system picker and the inbox; dialogs that need a
  synchronous path (Color Settings › load ICC profile, Book/Library/Data Merge sources, Word
  import options) have no picker on Android, as on the web.
- No TCP control channel, no file drag-and-drop, no system clipboard images.
- Placed graphics are embedded (no linked files); the Links panel has nothing to relink.
- System fonts come from `/system/fonts`; the embedded craft-fonts set covers the rest.
- The InDesign-style layout needs a tablet-sized screen; on a phone's cover screen it is cramped.
