#!/bin/bash
# Быстрый скрипт для тестирования

set -e

# Проверяем bootimage
if ! command -v cargo-bootimage &> /dev/null; then
    echo "⚙️  bootimage не установлен, устанавливаем..."
    cargo install bootimage --quiet
fi

# Собираем образ
echo "🔨 Сборка bootimage..."
cargo bootimage --quiet

# Копируем в корень
echo "📦 Копирование образа..."
cp target/x86_64-rustos/debug/bootimage-rustos.bin rustos.bin

# Создаем диск если нужно
if [ ! -f disk.img ]; then
    echo "📀 Создаём disk.img..."
    dd if=/dev/zero of=disk.img bs=1M count=100 2>/dev/null
    mkfs.fat -F 32 disk.img >/dev/null 2>&1
fi

echo ""
echo "🚀 Запуск (вывод в терминал, можно копировать)..."
echo "   Для выхода: Ctrl+A, затем X"
echo ""
echo "========================================"
echo ""

# Запускаем
exec qemu-system-x86_64 \
    -drive file=rustos.bin,format=raw,if=ide,index=0 \
    -drive file=disk.img,format=raw,if=ide,index=1 \
    -nographic \
    -no-reboot
