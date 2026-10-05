#!/usr/bin/env bash
# Builds lattice-client.exe for Windows from WSL and copies it to a Windows
# folder. No Windows toolchain and no sudo: Rust's x86_64-pc-windows-gnullvm
# target, with llvm-mingw (unpacked in ~/.local/opt) as linker and C compiler
# (ring builds C).
#
#   scripts/client-windows.sh [DEST]      DEST: a WSL path [/mnt/c/lattice]
#
# Then on Windows: C:\lattice\lattice-client.exe --server <WSL IP>:40000
# (`hostname -I` in WSL; the server binds 0.0.0.0 by default).
# Env: LLVM_MINGW (default: the newest ~/.local/opt/llvm-mingw-*). To get one:
#   https://github.com/mstorsjo/llvm-mingw/releases, the ucrt-ubuntu x86_64 tarball.
set -euo pipefail
cd "$(dirname "$0")/../client"

LLVM_MINGW=${LLVM_MINGW:-$(ls -d ~/.local/opt/llvm-mingw-* 2>/dev/null | sort | tail -1)}
[ -d "${LLVM_MINGW:-}" ] || { echo "no llvm-mingw in ~/.local/opt (see the header of $0)" >&2; exit 1; }
target=x86_64-pc-windows-gnullvm
export PATH="$LLVM_MINGW/bin:$PATH"
export CC_x86_64_pc_windows_gnullvm=x86_64-w64-mingw32-clang
export CXX_x86_64_pc_windows_gnullvm=x86_64-w64-mingw32-clang++
export AR_x86_64_pc_windows_gnullvm=llvm-ar
export CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER=x86_64-w64-mingw32-clang
rustup target add "$target" > /dev/null
# Without debug info: the workspace keeps it for profiling, and it makes the
# exe ~850 MB.
CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_PROFILE_RELEASE_STRIP=true cargo build --release --target "$target" --bin lattice-client

exe=target/$target/release/lattice-client.exe
dest=${1:-/mnt/c/lattice}
mkdir -p "$dest"
cp "$exe" "$dest/"
# The llvm-mingw runtime DLLs the exe imports (libunwind, libc++), beside it.
for dll in $(llvm-objdump -p "$exe" | awk '/DLL Name:/ {print $3}'); do
  src="$LLVM_MINGW/x86_64-w64-mingw32/bin/$dll"
  [ -f "$src" ] && cp "$src" "$dest/"
done
# The models: Bevy looks for assets/ beside the exe.
mkdir -p "$dest/assets/models"
cp assets/models/*.glb "$dest/assets/models/"
echo "built $(du -h "$exe" | cut -f1) -> $dest ($(ls "$dest" | tr '\n' ' '))"
echo "server's address from Windows: $(hostname -I | awk '{print $1}'):40000"
