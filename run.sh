#!/bin/bash
# Скрипт для запуска RustOS в QEMU

# Проверяем наличие образа в корне
if [ ! -f rustos.bin ]; then
    echo "⚠️  Образ rustos.bin не найден. Запускаем сборку..."
    ./build.sh
fi

# Создаем FAT32 диск для persistent storage, если его нет
if [ ! -f disk.img ]; then
    echo "📀 Создаём виртуальный FAT32 диск (100MB)..."
    dd if=/dev/zero of=disk.img bs=1M count=100 2>/dev/null
    mkfs.fat -F 32 disk.img >/dev/null 2>&1
    echo "✅ FAT32 диск создан: disk.img"
else
    echo "✓ Используем существующий disk.img"
fi

echo ""
echo "🚀 Запуск RustOS в QEMU..."
echo "   📀 Загрузочный диск: rustos.bin"
echo "   💾 FAT32 диск: disk.img (постоянное хранилище)"
echo "   Для выхода: Ctrl+A, затем X"
echo ""

# Запуск QEMU:
# - Загрузка с floppy (rustos.bin)
# - IDE Primary Master: FAT32 диск для данных
qemu-system-x86_64 \
    -fda rustos.bin \
    -drive format=raw,file=disk.img,if=ide,index=0 \
    -serial stdio \
    "$@"
