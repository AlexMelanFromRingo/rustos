# RustOS - Минимальное ядро на Rust

Минимальное операционное системное ядро, написанное на Rust, следуя урокам [Writing an OS in Rust](https://os.phil-opp.com/) от Phil Opp.

## Возможности

- Загружаемое ядро для x86_64
- Вывод текста в VGA буфер
- Поддержка макросов `print!` и `println!`
- Обработка паник
- Freestanding binary (не зависит от стандартной библиотеки)

## Требования

- Rust nightly (автоматически устанавливается через `rust-toolchain`)
- `rust-src` компонент: `rustup component add rust-src`
- `llvm-tools-preview`: `rustup component add llvm-tools-preview`
- `bootimage` tool: `cargo install bootimage`
- QEMU (для запуска): `sudo apt install qemu-system-x86`

## Сборка

### Установка зависимостей

```bash
# Добавить необходимые компоненты Rust
rustup component add rust-src llvm-tools-preview

# Установить bootimage
cargo install bootimage
```

### Сборка ядра

```bash
# Простая сборка и создание образа rustos.bin в корне проекта
./build.sh

# Или вручную:
cargo bootimage
cp target/x86_64-rustos/debug/bootimage-rustos.bin rustos.bin
```

## Запуск

### С помощью QEMU

```bash
# Самый простой способ - использовать скрипт (автоматически соберет если нужно)
./run.sh

# Или напрямую с помощью QEMU (образ в корне проекта)
qemu-system-x86_64 -drive format=raw,file=rustos.bin

# Или через cargo run (использует настройки из .cargo/config.toml)
cargo run
```

### Запуск на настоящем оборудовании

**ВНИМАНИЕ:** Это перезапишет данные на USB-накопителе!

```bash
# Создать загружаемый образ
./build.sh

# Записать на USB (замените /dev/sdX на ваше устройство)
sudo dd if=rustos.bin of=/dev/sdX && sync
```

## Тестирование

Проект включает систему тестирования для bare-metal окружения:

```bash
# Запустить все тесты
./test.sh
```

### Типы тестов

- **Unit тесты** - тесты для VGA buffer и других модулей
- **Integration тесты** - тесты загрузки и базовой функциональности
- **Should panic тесты** - тесты корректной обработки паник

Тестовая система использует:
- Custom test framework (без стандартной библиотеки)
- Serial port для вывода результатов
- QEMU isa-debug-exit device для exit кодов
- Автоматическое создание загрузочных образов для каждого теста

## Структура проекта

```
rustos/
├── src/
│   ├── main.rs              # Точка входа ядра
│   ├── lib.rs               # Библиотека для переиспользования кода
│   ├── vga_buffer.rs        # VGA текстовый режим
│   ├── serial.rs            # Serial port для тестов
│   ├── qemu.rs              # QEMU exit device
│   └── interrupts.rs        # CPU exception handlers (IDT)
├── tests/
│   ├── basic_boot.rs        # Integration тест загрузки
│   └── should_panic.rs      # Тест обработки паник
├── .cargo/
│   └── config.toml          # Настройки сборки Cargo
├── x86_64-rustos.json       # Спецификация целевой платформы
├── Cargo.toml               # Манифест проекта
├── rust-toolchain           # Указывает использовать nightly
├── build.sh                 # Скрипт сборки (создает rustos.bin)
├── run.sh                   # Скрипт запуска в QEMU
├── test.sh                  # Скрипт запуска тестов
├── rustos.bin               # Загружаемый образ (создается build.sh)
└── README.md                # Этот файл
```

## Стадии разработки

Проект развивается поэтапно, каждая стадия в отдельной ветке:

- **Stage 0: Standalone Binary** ✅ - Минимальное ядро с VGA выводом
- **Stage 1: Testing** ✅ - Система тестирования для ядра (custom test framework, serial output, QEMU exit device)
- **Stage 2: CPU Exceptions** ✅ - Обработка исключений процессора (IDT, breakpoint, double fault)
- **Stage 3: Hardware Interrupts** ✅ - Обработка прерываний (PIC, таймер, клавиатура)
- **Stage 4: Memory Management** 📋 - Управление памятью
- **Stage 5: Heap Allocation** 📋 - Динамическая память
- **Stage 6: Multitasking** 📋 - Многозадачность

## Как это работает

### Freestanding Binary

Ядро является freestanding бинарником, что означает:
- `#![no_std]` - не использует стандартную библиотеку Rust
- `#![no_main]` - не использует стандартную точку входа
- Определяет собственную функцию `_start()` как точку входа
- Определяет собственный обработчик паник

### VGA Text Mode

Ядро выводит текст напрямую в VGA текстовый буфер по адресу `0xb8000`:
- Каждый символ занимает 2 байта (ASCII код + цветовой код)
- Поддержка 16 цветов для текста и фона
- Автоматическая прокрутка при заполнении экрана
- Безопасная абстракция поверх небезопасных операций

### Загрузка

Проект использует пакет `bootloader` для:
- Настройки длинного режима (64-bit)
- Создания таблиц страниц
- Загрузки ядра и передачи управления на `_start()`

## Обучающие материалы

Этот проект основан на отличных уроках:
- [Writing an OS in Rust](https://os.phil-opp.com/)
- [GitHub репозиторий](https://github.com/phil-opp/blog_os)

## Дорожная карта

Следующие возможности для реализации:
- ✅ VGA текстовый буфер с println! макросом
- ✅ Система тестирования (unit и integration тесты)
- ✅ CPU исключения (IDT, breakpoint, double fault)
- ✅ Аппаратные прерывания (PIC, таймер, клавиатура)
- 📋 Управление памятью (paging, frame allocator)
- 📋 Динамическая память (heap allocator)
- 📋 Многозадачность (async/await, cooperative multitasking)
- 📋 Файловая система
- 📋 Системные вызовы
- 📋 Пользовательские процессы

## Лицензия

Этот проект создан в образовательных целях.
