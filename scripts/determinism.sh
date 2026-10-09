#!/usr/bin/env bash
# Checks that the shared rules (lattice-game) give the same bits on Linux,
# where the server runs, and on Windows, where the client runs: the client
# predicts its own movement and the server checks it bit for bit. Runs
# game/examples/determinism (hashes of the world, a long movement replay,
# the weapon math and the codecs) on both and compares.
#
#   scripts/determinism.sh [seed]
#
# Run it after touching lattice-game, and before trusting new physics (M4's
# vehicles). The Windows build is cross-compiled like scripts/client-windows.sh
# (llvm-mingw in ~/.local/opt, or LLVM_MINGW) and runs through WSL interop
# from C:\lattice\determinism.
set -euo pipefail
cd "$(dirname "$0")/.."

seed=${1:-1}
LLVM_MINGW=${LLVM_MINGW:-$(ls -d ~/.local/opt/llvm-mingw-* 2> /dev/null | sort | tail -1 || true)}
[ -d "${LLVM_MINGW:-}" ] || { echo "no llvm-mingw in ~/.local/opt (see scripts/client-windows.sh)" >&2; exit 1; }
target=x86_64-pc-windows-gnullvm

cargo build --release -q -p lattice-game --example determinism
(
  export PATH="$LLVM_MINGW/bin:$PATH"
  export CC_x86_64_pc_windows_gnullvm=x86_64-w64-mingw32-clang
  export AR_x86_64_pc_windows_gnullvm=llvm-ar
  export CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER=x86_64-w64-mingw32-clang
  rustup target add "$target" > /dev/null
  CARGO_PROFILE_RELEASE_DEBUG=0 cargo build --release -q --target "$target" -p lattice-game --example determinism
)

# Beside it, the llvm-mingw runtime DLLs it imports.
dir=/mnt/c/lattice/determinism
mkdir -p "$dir"
exe=target/$target/release/examples/determinism.exe
cp "$exe" "$dir/"
for dll in $("$LLVM_MINGW/bin/llvm-objdump" -p "$exe" | awk '/DLL Name:/ {print $3}'); do
  src="$LLVM_MINGW/x86_64-w64-mingw32/bin/$dll"
  [ -f "$src" ] && cp "$src" "$dir/"
done

linux=$(target/release/examples/determinism "$seed")
windows=$("$dir/determinism.exe" "$seed" | tr -d '\r')
paste -d' ' <(echo "$linux") <(echo "$windows") | awk '{printf "%-9s linux %s  windows %s  %s\n", $1, $2, $4, ($2 == $4 ? "same" : "DIFFERENT")}'
if [ "$linux" = "$windows" ]; then
  echo "Linux and Windows agree (seed $seed)"
else
  echo "Linux and Windows DIFFER (seed $seed): the shared rules aren't deterministic across platforms" >&2
  exit 1
fi
