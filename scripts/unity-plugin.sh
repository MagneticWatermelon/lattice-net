#!/usr/bin/env bash
# Builds the Unity client's native plugin (client-ffi: lattice_client.dll) for
# Windows from WSL, generates its C# bindings (gen-cs), and copies both into
# the Unity project. The two always travel together: the C# side refuses a
# plugin whose version or layout hash differs from its bindings'.
#
#   scripts/unity-plugin.sh [UNITY_PROJECT]   a WSL path [/mnt/c/Users/$USER/Projects/Lattice]
#
# The DLL goes to Assets/Plugins/x86_64/, the bindings to
# Assets/Lattice/Native/Generated/. In the editor, the client loads a copy of
# the DLL (Temp/LatticePlugin), so a rebuild can overwrite it while Unity runs:
# reload it with Lattice > Reload Plugin, or by entering Play mode.
#
# Like client-windows.sh: Rust's x86_64-pc-windows-gnullvm target with
# llvm-mingw (in ~/.local/opt) as linker and C compiler (ring builds C), and
# a static C runtime, so the DLL needs nothing beyond Windows' own DLLs.
set -euo pipefail
cd "$(dirname "$0")/.."

dest=${1:-${LATTICE_UNITY:-/mnt/c/Users/$USER/Projects/Lattice}}
[ -d "$dest/Assets" ] || { echo "no Unity project at $dest (no Assets/)" >&2; exit 1; }

LLVM_MINGW=${LLVM_MINGW:-$(ls -d ~/.local/opt/llvm-mingw-* 2>/dev/null | sort | tail -1)}
[ -d "${LLVM_MINGW:-}" ] || { echo "no llvm-mingw in ~/.local/opt (see scripts/client-windows.sh)" >&2; exit 1; }
target=x86_64-pc-windows-gnullvm
export PATH="$LLVM_MINGW/bin:$PATH"
export CC_x86_64_pc_windows_gnullvm=x86_64-w64-mingw32-clang
export CXX_x86_64_pc_windows_gnullvm=x86_64-w64-mingw32-clang++
export AR_x86_64_pc_windows_gnullvm=llvm-ar
export CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER=x86_64-w64-mingw32-clang
rustup target add "$target" > /dev/null
# No debug info (the workspace keeps it for profiling); the C runtime and
# libunwind linked in.
CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_PROFILE_RELEASE_STRIP=true RUSTFLAGS="-C target-feature=+crt-static" \
  cargo build --release --target "$target" -p lattice-client-ffi --lib

dll=target/$target/release/lattice_client.dll
imports=$(llvm-objdump -p "$dll" | awk '/DLL Name:/ {print $3}')
for d in $imports; do
  case "${d,,}" in
    kernel32.dll|ws2_32.dll|ntdll.dll|bcrypt.dll|bcryptprimitives.dll|advapi32.dll|userenv.dll|api-ms-win-*) ;;
    *) echo "lattice_client.dll imports $d, which Windows doesn't ship" >&2; exit 1 ;;
  esac
done

cargo build --release -p lattice-client-ffi --bin gen-cs
mkdir -p "$dest/Assets/Plugins/x86_64" "$dest/Assets/Lattice/Native/Generated"
cp "$dll" "$dest/Assets/Plugins/x86_64/lattice_client.dll"
target/release/gen-cs "$dest/Assets/Lattice/Native/Generated/LatticeNative.g.cs"
echo "plugin $(du -h "$dll" | cut -f1) and bindings -> $dest"
echo "server's address from Windows: $(hostname -I | awk '{print $1}'):40000"
