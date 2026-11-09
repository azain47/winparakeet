#!/usr/bin/env bash
# Cross-builds WinParakeet for Windows x64 from WSL with cargo-xwin, Vulkan GPU
# backend included, and lays out a runnable portable app in dist-windows/:
# winparakeet.exe, the transcribe.cpp/ggml DLLs and resources/. A `portable`
# marker keeps this build's settings, models and logs in dist-windows/Data/, so
# it never touches an installed WinParakeet.
#
# Run from Windows:
#   wsl -d Ubuntu -e bash /mnt/d/winparakeet/scripts/build-windows-wsl.sh [--run]
#
# One-time requirements inside WSL:
#   rustup target add x86_64-pc-windows-msvc
#   cargo-xwin, bun, cmake, ninja, glslc (shaderc)
#   LLVM with clang-cl, llvm-lib, llvm-rc and llvm-dlltool (e.g. apt llvm-21, clang-21)
# The first run also assembles a minimal Windows Vulkan SDK (headers, SPIRV-Headers,
# an import library for vulkan-1.dll) under ~/.cache/winparakeet-tools.
set -euo pipefail

RUN_AFTER_BUILD=0
for arg in "$@"; do
    case "$arg" in
        --run) RUN_AFTER_BUILD=1 ;;
        *) echo "usage: $0 [--run]" >&2; exit 2 ;;
    esac
done

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET=x86_64-pc-windows-msvc
TOOLS="${WINPARAKEET_TOOLS_DIR:-$HOME/.cache/winparakeet-tools}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/winparakeet-target}"
# Same Vulkan SDK version as the Windows CI build.
VULKAN_SDK_VERSION=1.4.309.0

# `wsl -e bash` starts a non-login shell: add the usual tool locations.
for dir in "$HOME/.local/share/mise/shims" "$HOME/.cargo/bin" /home/linuxbrew/.linuxbrew/bin; do
    [[ -d "$dir" ]] && PATH="$dir:$PATH"
done
# A mise-installed bun with no global default has a shim that refuses to run.
# Pin the newest installed version for this process tree: the Tauri CLI runs
# under node, whose mise shim puts the shims dir back in front for the
# `bun run build` it spawns.
if ! bun --version >/dev/null 2>&1; then
    bun_version="$(ls "$HOME/.local/share/mise/installs/bun" 2>/dev/null | grep -E '^[0-9]' | sort -V | tail -n 1)"
    [[ -n "$bun_version" ]] && export MISE_BUN_VERSION="$bun_version"
fi
LLVM_BIN="${LLVM_BIN:-$(ls -d /usr/lib/llvm-*/bin 2>/dev/null | sort -V | tail -n 1)}"
if [[ -z "$LLVM_BIN" || ! -x "$LLVM_BIN/clang-cl" ]]; then
    echo "error: no LLVM with clang-cl found; install llvm + clang or set LLVM_BIN" >&2
    exit 1
fi

# rustc passes `-flavor link` itself, which rustup's gcc-ld/lld-link wrapper
# duplicates; a plain lld-link name for rust-lld works for rustc and clang-cl.
mkdir -p "$TOOLS/bin"
HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
ln -sf "$(rustc --print sysroot)/lib/rustlib/$HOST_TRIPLE/bin/rust-lld" "$TOOLS/bin/lld-link"
export PATH="$TOOLS/bin:$LLVM_BIN:$PATH"

for tool in bun cargo cargo-xwin cmake ninja glslc clang-cl llvm-lib llvm-rc llvm-dlltool; do
    command -v "$tool" >/dev/null || { echo "error: $tool not found on PATH" >&2; exit 1; }
done

# ggml-vulkan needs Vulkan headers, an import library for the Windows loader
# (vulkan-1.dll), a host glslc, and SPIRV-Headers: found as a CMake package, but
# included through the Vulkan include dir, as in LunarG's SDK.
SDK="$TOOLS/vulkan-sdk-$VULKAN_SDK_VERSION"
if [[ ! -f "$SDK/Lib/vulkan-1.lib" || ! -f "$SDK/Include/spirv/unified1/spirv.hpp" ]]; then
    echo "==> Assembling Windows Vulkan SDK $VULKAN_SDK_VERSION in $SDK"
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' EXIT
    tag="vulkan-sdk-$VULKAN_SDK_VERSION"
    curl -fsSL "https://github.com/KhronosGroup/Vulkan-Headers/archive/refs/tags/$tag.tar.gz" | tar -xz -C "$work"
    curl -fsSL "https://github.com/KhronosGroup/SPIRV-Headers/archive/refs/tags/$tag.tar.gz" | tar -xz -C "$work"
    curl -fsSL -o "$work/vulkan-1.def" "https://raw.githubusercontent.com/KhronosGroup/Vulkan-Loader/$tag/loader/vulkan-1.def"
    rm -rf "$SDK"
    mkdir -p "$SDK/Include" "$SDK/Lib" "$SDK/Bin"
    cp -r "$work/Vulkan-Headers-$tag/include/." "$SDK/Include/"
    cmake -S "$work/SPIRV-Headers-$tag" -B "$work/spirv-build" -G Ninja \
        -DCMAKE_INSTALL_PREFIX="$SDK" -DCMAKE_INSTALL_INCLUDEDIR=Include \
        -DSPIRV_HEADERS_ENABLE_TESTS=OFF >/dev/null
    cmake --install "$work/spirv-build" >/dev/null
    llvm-dlltool -m i386:x86-64 -d "$work/vulkan-1.def" -l "$SDK/Lib/vulkan-1.lib"
fi
ln -sf "$(command -v glslc)" "$SDK/Bin/glslc"
export VULKAN_SDK="$SDK"

# transcribe-cpp-sys configure overrides, all specific to clang-cl/lld-link:
# - Vulkan pieces from the SDK assembled above.
# - cargo-xwin's CMake toolchain empties CMAKE_<LANG>_STANDARD_LIBRARIES, which
#   drops advapi32 (ggml-cpu reads the CPU name from the registry).
# - ggml's per-ISA CPU modules (GGML_CPU_ALL_VARIANTS) pass MSVC /arch flags
#   only, so clang-cl rejects the AVX-VNNI/AMX variants. Build one AVX2/FMA/F16C/
#   BMI2 module instead; any x86 CPU from Haswell / Zen 1 on runs it. The CI
#   build (MSVC) still ships every variant.
cmake_args=(
    "-DVulkan_INCLUDE_DIR=$SDK/Include"
    "-DVulkan_LIBRARY=$SDK/Lib/vulkan-1.lib"
    "-DVulkan_GLSLC_EXECUTABLE=$SDK/Bin/glslc"
    -DCMAKE_SHARED_LINKER_FLAGS=advapi32.lib
    -DCMAKE_MODULE_LINKER_FLAGS=advapi32.lib
    -DGGML_CPU_ALL_VARIANTS=OFF
    -DGGML_SSE42=ON -DGGML_AVX=ON -DGGML_AVX2=ON -DGGML_FMA=ON -DGGML_F16C=ON -DGGML_BMI2=ON
)
export TRANSCRIBE_CMAKE_ARGS="${cmake_args[*]}"

# Compiler, archiver, linker, SDK include/lib paths and the CMake toolchain.
eval "$(cargo xwin env --target "$TARGET")"
# cargo-xwin also sets `-C linker-flavor=lld-link -Lnative=...` as target
# rustflags. Cargo hands those to build scripts as CARGO_ENCODED_RUSTFLAGS, and
# ferrous-opencc's cbindgen runs a nested `cargo metadata` that applies them to
# the host and fails. The LINKER=lld-link and LIB variables above cover both.
unset CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS

cd "$REPO"
echo "==> Installing frontend dependencies"
bun install --frozen-lockfile
echo "==> Building ($TARGET, target dir $CARGO_TARGET_DIR)"
bun tauri build --target "$TARGET" --no-bundle

OUT="$REPO/dist-windows"
EXE="$CARGO_TARGET_DIR/$TARGET/release/winparakeet.exe"
# A running copy keeps the exe and its DLLs locked. One running as administrator
# can only be closed from an elevated shell.
if command -v taskkill.exe >/dev/null && tasklist.exe /FI "IMAGENAME eq winparakeet.exe" 2>/dev/null | grep -qi winparakeet.exe; then
    taskkill.exe /IM winparakeet.exe /F >/dev/null || {
        echo "error: couldn't close the running WinParakeet; quit it from the tray (or run this from an elevated terminal) and build again" >&2
        exit 1
    }
    sleep 1
fi
mkdir -p "$OUT"
cp "$EXE" "$OUT/"
cp src-tauri/transcribe-libs/*.dll "$OUT/"
rm -rf "$OUT/resources"
cp -r src-tauri/resources "$OUT/resources"
# portable.rs: a `portable` marker with this text keeps data in $OUT/Data.
printf 'Handy Portable Mode' >"$OUT/portable"

win_out="$(wslpath -w "$OUT" 2>/dev/null || echo "$OUT")"
echo "==> Built $win_out\\winparakeet.exe"
if [[ "$RUN_AFTER_BUILD" == 1 ]]; then
    (cd "$OUT" && cmd.exe /c start "" winparakeet.exe >/dev/null 2>&1)
fi
