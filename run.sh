#!/bin/bash
# Скрипт для запуска RustOS в QEMU

# Проверяем наличие образа в корне
if [ ! -f rustos.bin ]; then
    echo "⚠️  Образ rustos.bin не найден. Запускаем сборку..."
    ./build.sh
fi

echo "🚀 Запуск RustOS в QEMU..."
echo "   Для выхода: Ctrl+A, затем X"
echo ""

qemu-system-x86_64 -drive format=raw,file=rustos.bin "$@"
