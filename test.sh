#!/bin/bash
# Скрипт для запуска тестов RustOS

set -e

echo "🧪 Запуск тестов RustOS..."
echo ""

# Build all tests
echo "📦 Сборка тестов..."
cargo build --tests --target x86_64-rustos.json

# Create bootimages for tests
echo "🔨 Создание загрузочных образов..."
export CARGO_MANIFEST_DIR=/home/user/rustos

for test_exec in target/x86_64-rustos/debug/deps/*-*[!.d]; do
    if [ -f "$test_exec" ] && [ -x "$test_exec" ]; then
        test_name=$(basename "$test_exec")
        # Skip if it's the main binary
        if [[ ! "$test_name" =~ ^rustos-  ]]; then
            echo "  - Creating bootimage for $test_name..."
            bootimage runner "$test_exec" > /dev/null 2>&1 || true
        fi
    fi
done

echo ""
echo "✅ Образы созданы"
echo ""

# Run tests
FAILED=0
PASSED=0

for bootimage_file in target/x86_64-rustos/debug/deps/bootimage-*.bin; do
    if [ -f "$bootimage_file" ]; then
        test_name=$(basename "$bootimage_file" .bin | sed 's/bootimage-//')

        echo -n "🧪 Тест: $test_name... "

        if qemu-system-x86_64 \
            -drive format=raw,file="$bootimage_file" \
            -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
            -serial stdio \
            -display none \
            > /dev/null 2>&1; then
            echo "❌ FAILED (unexpected exit code 0)"
            FAILED=$((FAILED + 1))
        else
            exit_code=$?
            if [ $exit_code -eq 33 ]; then
                echo "✅ PASSED"
                PASSED=$((PASSED + 1))
            else
                echo "❌ FAILED (exit code: $exit_code)"
                FAILED=$((FAILED + 1))
            fi
        fi
    fi
done

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "📊 Результаты: $PASSED пройдено, $FAILED провалено"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

if [ $FAILED -eq 0 ]; then
    echo "🎉 Все тесты успешно пройдены!"
    exit 0
else
    echo "❌ Некоторые тесты провалились"
    exit 1
fi
