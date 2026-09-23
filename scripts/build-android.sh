#!/usr/bin/env bash
# The Android and JVM counterpart of build-xcframework.sh: the Kotlin bindings
# from the same UDL, the shared library per Android ABI for an app's jniLibs/,
# and a host build of the same library for a JVM test run.
#
#   lib/target/kotlin/uniffi/yniffi/yniffi.kt      the bindings (uniffi-bindgen --language kotlin)
#   lib/target/jniLibs/<abi>/libuniffi_yniffi.so    per ABI, when ANDROID_NDK_HOME is set
#   lib/target/release/libuniffi_yniffi.{so,dylib}  the host build a JVM test run loads
# (under $CARGO_TARGET_DIR instead of lib/target when that is set).
#
# The Android legs need cargo-ndk (`cargo install cargo-ndk`) and an NDK; with
# no ANDROID_NDK_HOME they are skipped, printed as such, and the host build
# still runs, so a Linux CI job without an NDK can run the JVM tier. Every
# cargo build passes --locked, as build-xcframework.sh does: the lock is what
# makes the .so a function of the tree.
set -euo pipefail
cd "$(dirname "$0")/../lib"

echo "▸ Generate Kotlin bindings"
cargo run --locked --features=uniffi/cli --bin uniffi-bindgen generate \
    ./src/yniffi.udl --language kotlin --out-dir target/kotlin

if [ -n "${ANDROID_NDK_HOME:-}" ]; then
    echo "▸ Build for arm64-v8a, armeabi-v7a, x86_64 into target/jniLibs"
    cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 -o target/jniLibs \
        build --locked --release
else
    echo "▸ ANDROID_NDK_HOME is unset: skipping the Android ABIs, building the host library only"
fi

echo "▸ Build the host library"
cargo build --locked --release
ls "${CARGO_TARGET_DIR:-target}"/release/libuniffi_yniffi.*
