#!/bin/bash
# Скрипт для запуска RustOS в QEMU

echo "Запуск RustOS в QEMU..."
echo "Для выхода нажмите Ctrl+A, затем X"
echo ""

qemu-system-x86_64 -drive format=raw,file=target/x86_64-rustos/debug/bootimage-rustos.bin "$@"
