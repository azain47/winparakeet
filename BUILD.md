# Building WinParakeet

WinParakeet targets Windows x64. There are three ways to build it:

- [Cross-compile from WSL](#cross-compile-from-wsl): no Windows toolchain needed; a runnable portable build for local work.
- [Native Windows build](#native-windows-build): Visual Studio Build Tools, CMake and the Vulkan SDK on Windows; needed for installers.
- [GitHub Actions](#github-actions): installers for every push to `main`, and draft releases.

## Cross-compile from WSL

`scripts/build-windows-wsl.sh` builds with [cargo-xwin](https://github.com/rust-cross/cargo-xwin) (clang-cl, lld and the MSVC CRT/Windows SDK it downloads), including the Vulkan GPU backend.

One-time setup inside WSL:

- Rust with the target: `rustup target add x86_64-pc-windows-msvc`
- cargo-xwin, for example `cargo install --locked cargo-xwin`
- [Bun](https://bun.sh)
- LLVM with `clang-cl`, `llvm-lib`, `llvm-rc` and `llvm-dlltool` (Ubuntu: `sudo apt install clang llvm`)
- `cmake`, `ninja` and `glslc` (Homebrew: `brew install cmake ninja shaderc`; apt: `cmake ninja-build glslc`)

Build, from Windows:

```powershell
wsl -d Ubuntu -e bash /mnt/d/winparakeet/scripts/build-windows-wsl.sh --run
```

or from the repository inside WSL: `bash scripts/build-windows-wsl.sh`.

The script:

1. On first run, assembles a minimal Windows Vulkan SDK under `~/.cache/winparakeet-tools`: Vulkan-Headers and SPIRV-Headers, plus an import library for `vulkan-1.dll` generated from the Vulkan loader's export list. The version matches the CI build.
2. Runs `tauri build --target x86_64-pc-windows-msvc --no-bundle` with cargo-xwin's environment. Build output goes to `~/.cache/winparakeet-target` (`CARGO_TARGET_DIR` overrides it).
3. Lays out `dist-windows/`: `winparakeet.exe`, the transcribe.cpp and ggml DLLs, `resources/`, and a `portable` marker, so settings, models and logs stay in `dist-windows/Data/`. A running copy is closed first; `--run` starts the new one.

`--check` instead only type-checks the Rust code for Windows, tests included, reusing the native libraries the last build made: about a minute instead of a full build.

How this build differs from the CI build:

- One CPU backend module built for AVX2/FMA/F16C/BMI2 (Intel Haswell or AMD Zen and newer). ggml's per-ISA modules pass MSVC `/arch` flags only, and clang-cl rejects the AVX-VNNI and AMX ones; CI builds with MSVC and ships all of them.
- No installer and no app-local VC++ runtime. It runs on any machine with the Visual C++ 2015-2022 redistributable, which almost every Windows 10/11 machine has.

`LLVM_BIN` selects an LLVM install other than the newest `/usr/lib/llvm-*`; `WINPARAKEET_TOOLS_DIR` moves the SDK cache.

## Native Windows build

Requirements:

- [Rust](https://rustup.rs/) (stable, MSVC toolchain) and [Bun](https://bun.sh/)
- Visual Studio 2022 Build Tools with the C++ workload
- [CMake](https://cmake.org/download/) on `PATH`: `winget install Kitware.CMake`
- The [Vulkan SDK](https://vulkan.lunarg.com/sdk/home): `winget install KhronosGroup.VulkanSDK`, then open a new terminal so `VULKAN_SDK` is set

Then:

```powershell
bun install
bun run tauri dev                  # development build with hot reload
bun run tauri build --no-bundle    # release executable only
bun run tauri build                # NSIS and MSI installers
```

A full `tauri build` also signs the updater artifacts, so it needs the updater key in `TAURI_SIGNING_PRIVATE_KEY` (and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`). Without it, add `--config '{"bundle":{"createUpdaterArtifacts":false}}'`.

The Vulkan backend's shader generator nests deep CMake build trees. transcribe-cpp-sys builds through a short NTFS junction under `%LOCALAPPDATA%\tcs` to stay under the 260-character path limit. If junctions are blocked and the build fails with `MSB3491`, `FTK1011` or `MSB6003`, set a short target directory, for example `$env:CARGO_TARGET_DIR = "C:\h"`.

## GitHub Actions

`.github/workflows/windows.yml` builds the NSIS and MSI installers on `windows-latest` for every push to `main` and every pull request, smoke-tests both packages (`winparakeet.exe --list-devices`), and uploads them as the `winparakeet-windows-x64` artifact.

Running the workflow manually with **release** checked creates a draft GitHub release for the version in `src-tauri/tauri.conf.json`, with the installers and the updater manifest (`latest.json`). The updater artifacts are signed with the key in the `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` secrets; the matching public key is `plugins.updater.pubkey` in `tauri.conf.json`.

## App icon

The icon source is `src-tauri/icons/app-icon.svg`. Regenerate every size with:

```bash
bun tauri icon src-tauri/icons/app-icon.svg
```

The tray icons in `src-tauri/resources/` are 64×64 renders of the glyphs in `src/components/icons/TrayIcon.tsx`: white for dark taskbars, `#111111` for light ones (`*_dark.png`), and `#16A34A` for the colored Linux set.
