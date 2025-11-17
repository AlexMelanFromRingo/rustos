#!/bin/bash
# Скрипт для сборки RustOS и копирования образа в корень проекта

set -e

echo "🔨 Сборка RustOS ядра..."
cargo bootimage

echo "📦 Копирование bootimage в корень проекта..."
cp target/x86_64-rustos/debug/bootimage-rustos.bin rustos.bin

echo "✅ Готово! Образ: rustos.bin ($(du -h rustos.bin | cut -f1))"
echo ""
echo "Для запуска: ./run.sh или qemu-system-x86_64 -drive format=raw,file=rustos.bin"
