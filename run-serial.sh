#!/bin/bash
# Запуск с serial output в stdio

if [ ! -f rustos.bin ]; then
    echo "Error: rustos.bin not found"
    exit 1
fi

if [ ! -f disk.img ]; then
    echo "Creating disk.img..."
    dd if=/dev/zero of=disk.img bs=1M count=100 2>/dev/null
    mkfs.fat -F 32 disk.img >/dev/null 2>&1
fi

echo "Starting QEMU with serial output to terminal..."
echo "Press Ctrl+C to exit"
echo ""
echo "========================================="
echo ""

# Используем serial stdio и оставляем графическое окно
exec qemu-system-x86_64 \
    -drive file=rustos.bin,format=raw,if=ide,index=0 \
    -drive file=disk.img,format=raw,if=ide,index=1 \
    -serial stdio \
    -no-reboot
