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

# Запуск QEMU с двумя IDE дисками:
# - Primary Master (index=0): rustos.bin - загрузочный диск
# - Primary Slave (index=1): disk.img - FAT32 для данных
# ATA драйвер настроен на чтение Slave (0xB0), не Master (0xA0)
# Графическое окно для работы с shell
qemu-system-x86_64 \
    -drive file=rustos.bin,format=raw,if=ide,index=0 \
    -drive file=disk.img,format=raw,if=ide,index=1 \
    "$@"
