#!/bin/bash
# Build hello.rs as static ELF binary for RustOS

set -e

echo "Building hello.rs..."

# Compile to object file
rustc --edition 2021 \
    --crate-type bin \
    --target x86_64-unknown-none \
    -C opt-level=s \
    -C panic=abort \
    -C relocation-model=static \
    -C link-arg=-nostartfiles \
    -C link-arg=-static \
    -C link-arg=-Wl,--build-id=none \
    -C link-arg=-Wl,-Ttext=0x400000 \
    -o hello.elf \
    hello.rs

echo "Stripping debug symbols..."
strip hello.elf

echo "ELF info:"
file hello.elf
size hello.elf
readelf -h hello.elf | grep Entry

echo ""
echo "Build complete: hello.elf"
echo "Size: $(stat -c%s hello.elf) bytes"
