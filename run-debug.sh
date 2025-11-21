#!/bin/bash
# Отладочный запуск с выводом в терминал

if [ ! -f rustos.bin ]; then
    echo "Error: rustos.bin not found. Run ./build.sh first"
    exit 1
fi

if [ ! -f disk.img ]; then
    echo "Creating disk.img..."
    dd if=/dev/zero of=disk.img bs=1M count=100 2>/dev/null
    mkfs.fat -F 32 disk.img >/dev/null 2>&1
fi

echo "Starting QEMU in debug mode..."
echo "All output will appear in THIS terminal"
echo "Press Ctrl+C to exit"
echo ""
echo "========================================="
echo ""

# Запуск с явным serial и без графики
exec qemu-system-x86_64 \
    -drive file=rustos.bin,format=raw,if=ide,index=0 \
    -drive file=disk.img,format=raw,if=ide,index=1 \
    -serial mon:stdio \
    -display none \
    -no-reboot
