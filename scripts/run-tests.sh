#!/usr/bin/env bash
# Build the in-kernel test binary and run it under QEMU.
# Exits 0 on all-pass, non-zero on any test failure.
#
# The test runner lives in tests/kernel_tests.rs and is wired in
# Cargo.toml as a `[[bin]] kernel_tests` so it builds with the same
# bootloader pipeline as the main kernel.  After the kernel exits
# via the QEMU debug-exit port, bootimage's runner translates the
# port byte (`0x10 << 1 | 1` = 33 for success, `0x11 << 1 | 1` = 35
# for any panic) into a normal shell exit code (0 / 1).

set -e
cd "$(dirname "$0")/.."

echo "[1/2] Building kernel_tests…"
cargo +nightly bootimage --release -Z json-target-spec --bin kernel_tests >/dev/null

IMG="target/x86_64-rustos/release/bootimage-kernel_tests.bin"
echo "[2/2] Running tests under QEMU…"

# QEMU exit code: 33 = success, 35 = test-panic, anything else = QEMU error.
set +e
timeout 120 qemu-system-x86_64 \
  -drive "format=raw,file=$IMG" \
  -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
  -nographic -no-reboot -display none \
  -serial mon:stdio < /dev/null
QEMU_EXIT=$?
set -e

case "$QEMU_EXIT" in
  33) echo "kernel_tests: PASS"; exit 0 ;;
  35) echo "kernel_tests: FAIL (assertion panicked)"; exit 1 ;;
  124) echo "kernel_tests: TIMED OUT"; exit 1 ;;
  *)  echo "kernel_tests: QEMU exited $QEMU_EXIT"; exit "$QEMU_EXIT" ;;
esac
