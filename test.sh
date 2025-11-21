#!/bin/bash
# Быстрый скрипт для тестирования с выводом в терминал

set -e

echo "🔨 Пересборка ядра..."
cargo build --quiet

echo ""
echo "🚀 Запуск в текстовом режиме (вывод можно копировать)..."
echo "   Для выхода: Ctrl+A, затем X"
echo "   Или просто Ctrl+C"
echo ""
echo "================================"
echo ""

# Проверяем что disk.img существует
if [ ! -f disk.img ]; then
    echo "📀 Создаём disk.img..."
    dd if=/dev/zero of=disk.img bs=1M count=100 2>/dev/null
    mkfs.fat -F 32 disk.img >/dev/null 2>&1
fi

# Запускаем с явным форматом и выводом в терминал
qemu-system-x86_64 \
    -drive file=rustos.bin,format=raw,if=ide,index=0 \
    -drive file=disk.img,format=raw,if=ide,index=1 \
    -nographic \
    -no-reboot \
    -no-shutdown
