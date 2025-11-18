# RustOS - Минимальное ядро на Rust

Минимальное операционное системное ядро, написанное на Rust, следуя урокам [Writing an OS in Rust](https://os.phil-opp.com/) от Phil Opp.

## Возможности

- ✅ Загружаемое ядро для x86_64
- ✅ Вывод текста в VGA буфер с поддержкой `print!` и `println!`
- ✅ VGA hardware cursor (мигающий курсор следует за текстом)
- ✅ Обработка CPU исключений (IDT, breakpoint, double fault)
- ✅ Аппаратные прерывания (PIC, таймер, клавиатура)
- ✅ Ввод с клавиатуры с поддержкой Backspace
- ✅ Виртуальная память (paging) с полным маппингом физической памяти
- ✅ Динамическая память (heap allocation) - `Box`, `Vec`, `String`
- ✅ Асинхронность (async/await) с кооперативной многозадачностью
- ✅ Управление питанием (shutdown/reboot через ACPI)
- ✅ Интерактивный shell с историей команд и Tab-автодополнением
- ✅ RAM disk - простая in-memory файловая система (ls, cat, write, rm)
- ✅ Система тестирования для bare-metal окружения
- ✅ Freestanding binary (не зависит от стандартной библиотеки)

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
- **Integration тесты** - тесты загрузки и базовой функциональности (basic_boot)
- **Should panic тесты** - тесты корректной обработки паник
- **Heap allocation тесты** - тесты динамической памяти (Box, Vec, много аллокаций)

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
│   ├── vga_buffer.rs        # VGA текстовый режим с hardware cursor
│   ├── serial.rs            # Serial port для тестов
│   ├── qemu.rs              # QEMU exit device
│   ├── interrupts.rs        # CPU exceptions и hardware interrupts (IDT, PIC)
│   ├── gdt.rs               # Global Descriptor Table с TSS
│   ├── memory.rs            # Управление памятью (paging, frame allocator)
│   ├── allocator.rs         # Heap allocator (fixed-size block)
│   ├── power.rs             # Управление питанием (shutdown/reboot via ACPI)
│   ├── shell.rs             # Интерактивный shell с автодополнением
│   ├── task/
│   │   ├── mod.rs           # Task infrastructure для async/await
│   │   ├── executor.rs      # Simple task executor
│   │   ├── keyboard.rs      # Async keyboard input stream
│   │   └── timer.rs         # Async timer
│   └── fs/
│       ├── mod.rs           # Filesystem module
│       └── ramdisk.rs       # RAM disk - in-memory файловая система
├── tests/
│   ├── basic_boot.rs        # Integration тест загрузки
│   ├── should_panic.rs      # Тест обработки паник
│   └── heap_allocation.rs   # Тесты heap allocation
├── .cargo/
│   └── config.toml          # Настройки сборки Cargo (включая alloc)
├── x86_64-rustos.json       # Спецификация целевой платформы
├── Cargo.toml               # Манифест проекта со всеми зависимостями
├── rust-toolchain           # Указывает использовать nightly
├── build.sh                 # Скрипт сборки (создает rustos.bin)
├── run.sh                   # Скрипт запуска в QEMU
├── test.sh                  # Скрипт запуска тестов
├── rustos.bin               # Загружаемый образ (создается build.sh)
└── README.md                # Этот файл
```

## Стадии разработки

Проект развивается поэтапно:

- **Stage 0: Standalone Binary** ✅ - Минимальное ядро с VGA выводом
- **Stage 1: Testing** ✅ - Система тестирования (custom framework, serial, QEMU exit)
- **Stage 2: CPU Exceptions** ✅ - Обработка исключений (IDT, breakpoint, double fault с GDT/TSS)
- **Stage 3: Hardware Interrupts** ✅ - Прерывания (PIC, таймер, клавиатура)
- **Stage 4: Paging** ✅ - Виртуальная память (page tables, OffsetPageTable, frame allocator)
- **Stage 5: Heap Allocation** ✅ - Динамическая память (heap, linked_list_allocator, Box/Vec/String)
- **Stage 6: Async/Await** ✅ - Кооперативная многозадачность (executor, waker, async tasks)
- **Stage 7: Improved Allocator** ✅ - Fixed-size block allocator (вместо linked list)
- **Stage 8: Power Management** ✅ - Shutdown/Reboot через ACPI
- **Stage 9: Interactive Shell** ✅ - Командная оболочка с историей
- **Stage 10: Shell Enhancements** ✅ - Arrow keys для истории, backspace, фильтрация
- **Stage 11: Tab Autocomplete** ✅ - Автодополнение команд
- **Stage 12: VFS** 📋 - Virtual File System (абстракция файловой системы)
- **Stage 13: RAM Disk** ✅ - In-memory файловая система (ls, cat, write, rm)
- **Stage 14: FAT32** 📋 - Чтение FAT32 с диска
- **Stage 15: Context Switching** 📋 - Переключение между процессами
- **Stage 16: Process Scheduler** 📋 - Планировщик процессов
- **Stage 17: User Space** 📋 - Запуск кода в ring 3

## Как это работает

### Freestanding Binary

Ядро является freestanding бинарником, что означает:
- `#![no_std]` - не использует стандартную библиотеку Rust
- `#![no_main]` - не использует стандартную точку входа
- Использует `entry_point!` макрос от bootloader для получения BootInfo
- Определяет собственный обработчик паник

### VGA Text Mode

Ядро выводит текст напрямую в VGA текстовый буфер по адресу `0xb8000`:
- Каждый символ занимает 2 байта (ASCII код + цветовой код)
- Поддержка 16 цветов для текста и фона
- Автоматическая прокрутка при заполнении экрана
- Hardware cursor через VGA порты 0x3D4/0x3D5
- Поддержка Backspace и расширенных символов
- Безопасная абстракция поверх небезопасных операций

### Управление питанием (Stage 8)

**Shutdown и Reboot через ACPI:**
- Shutdown через ACPI PM1a Control Block (запись в порт 0x604)
- Reboot через PS/2 контроллер (порт 0x64, команда 0xFE)
- Triple fault fallback если ACPI недоступен
- Безопасное завершение работы ядра

### Интерактивный Shell (Stages 9-11)

**Командная оболочка с расширенным функционалом:**
- Буфер ввода с поддержкой редактирования
- История команд (до 50 команд) с навигацией стрелками UP/DOWN
- Tab-автодополнение команд (префиксный поиск)
- Защита от удаления приглашения (backspace блокируется на пустом буфере)
- Фильтрация escape-последовательностей (только printable ASCII)
- Встроенные команды: help, clear, echo, version, uptime, time, meminfo, history, shutdown, reboot

### Файловая система (Stage 13)

**RAM Disk - In-Memory файловая система:**
- Максимум 64 файла по 4 КБ каждый (256 КБ всего)
- Имена файлов до 32 символов
- Операции: создание/перезапись (write), чтение (read), удаление (delete), список (list)
- Global Mutex-защищенный singleton (RAMDISK)
- Shell команды: ls, cat, write, rm
- Поддержка UTF-8 текста и бинарных данных (hex dump)

### Память

**Paging (Stage 4):**
- Использует 4-level page tables (x86_64)
- Полное маппирование физической памяти
- OffsetPageTable для удобного доступа
- BootInfoFrameAllocator для выделения фреймов

**Heap Allocation (Stage 5):**
- 100 KiB heap на виртуальном адресе 0x_4444_4444_0000
- linked_list_allocator для управления heap
- Доступны все типы из `alloc` crate (Box, Vec, String, Arc, Rc, etc.)

### Async/Await (Stage 6)

**Кооперативная многозадачность:**
- Task wrapper для Future с pinning
- Simple executor с Waker API
- ArrayQueue для готовых задач
- ScancodeStream для async keyboard input
- Оптимизация CPU (hlt когда нет задач)

### Прерывания

**CPU Exceptions (Stage 2):**
- Interrupt Descriptor Table (IDT)
- Обработчики breakpoint и double fault
- Global Descriptor Table (GDT) с Task State Segment (TSS)
- Interrupt Stack Table для безопасной обработки исключений

**Hardware Interrupts (Stage 3):**
- 8259 PIC (Programmable Interrupt Controller)
- Timer interrupt (IRQ 0)
- Keyboard interrupt (IRQ 1) с асинхронной обработкой
- Deadlock prevention (without_interrupts при работе с Mutex)

### Загрузка

Проект использует пакет `bootloader` для:
- Настройки длинного режима (64-bit)
- Создания начальных таблиц страниц
- Маппинга всей физической памяти
- Загрузки ядра и передачи управления на kernel_main() с BootInfo

## Зависимости

```toml
bootloader = "0.9.23"           # Bootloader с map_physical_memory
x86_64 = "0.14.2"               # x86_64 специфичные структуры
volatile = "0.2.6"              # Volatile read/write для VGA
spin = "0.5.2"                  # Spinlock Mutex для глобальных структур
lazy_static = "1.0"             # Статическая инициализация с spin_no_std
uart_16550 = "0.2.0"            # Serial port для тестов
pic8259 = "0.10.1"              # PIC управление
pc-keyboard = "0.7.0"           # Keyboard scancode обработка
linked_list_allocator = "0.9.0" # Heap allocator
crossbeam-queue = "0.3.8"       # Lock-free queue для async
conquer-once = "0.2.0"          # Lazy initialization
futures-util = "0.3.4"          # Stream utilities для async
```

## Обучающие материалы

Этот проект основан на отличных уроках:
- [Writing an OS in Rust](https://os.phil-opp.com/)
- [GitHub репозиторий](https://github.com/phil-opp/blog_os)

## Дорожная карта

### Реализовано ✅

- VGA текстовый буфер с println! макросом
- Hardware cursor и Backspace
- Система тестирования (unit и integration тесты)
- CPU исключения (IDT, breakpoint, double fault, GDT/TSS)
- Аппаратные прерывания (PIC, таймер, клавиатура)
- Управление памятью (paging, page tables, frame allocator)
- Динамическая память (fixed-size block allocator, Box/Vec/String)
- Асинхронность (async/await, executor, waker, cooperative multitasking)
- Управление питанием (shutdown/reboot через ACPI и PS/2)
- Интерактивный shell с историей команд и Tab-автодополнением
- RAM disk файловая система (ls, cat, write, rm)

### Планируется 📋

- Virtual File System (VFS) - абстракция для различных FS
- FAT32 драйвер (чтение файловой системы с диска)
- Процессы и потоки (настоящая многозадачность с context switching)
- Process Scheduler (планировщик процессов)
- Пользовательское пространство (user mode processes в ring 3)
- ACPI расширенная поддержка (обнаружение устройств)
- Сетевой стек (TCP/IP)
- Системные вызовы (syscall interface)
- ELF загрузчик (запуск программ)

## Производительность

Текущий размер образа: ~687 KB (включая bootloader)

## Лицензия

Этот проект создан в образовательных целях.
