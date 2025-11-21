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
- ✅ Интерактивный shell с редактируемым буфером и навигацией курсора
- ✅ Persistent command history - история команд сохраняется между перезагрузками на FAT32
- ✅ RAM disk - простая in-memory файловая система
- ✅ FAT32 - полная поддержка чтения/записи и вложенных директорий с автомонтированием при загрузке
- ✅ Расширенные Unix-утилиты (ls, cat, cp, mv, rm, touch, head, tail, wc, write, grep, pwd, du, find, tree, less, which)
- ✅ Полноценная навигация по директориям (cd, mkdir, rmdir) с поддержкой путей и вложенных папок
- ✅ Система алиасов команд (alias/unalias) для удобства работы
- ✅ Дополнительные утилиты: date, hostname, sleep - вдохновлено OpenComputers
- ✅ Упрощённые pipes (конвейеры) для связывания команд
- ✅ Встроенный текстовый редактор (edit) для построчного редактирования
- ✅ Переключение контекста (context switching) для многозадачности
- ✅ Планировщик процессов (round-robin) с вытесняющей многозадачностью
- ✅ Команды управления процессами (ps, kill)
- ✅ Система тестирования для bare-metal окружения (включая VFS тесты)
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
- **VFS тесты** - тесты файловой системы (создание, чтение, удаление, переполнение, ошибки)

Тестовая система использует:
- Custom test framework (без стандартной библиотеки)
- Serial port для вывода результатов
- QEMU isa-debug-exit device для exit кодов
- Автоматическое создание загрузочных образов для каждого теста

## Структура проекта

```
rustos/
├── src/
│   ├── main.rs              # Точка входа ядра (keyboard input с абсолютным cursor positioning)
│   ├── lib.rs               # Библиотека для переиспользования кода
│   ├── vga_buffer.rs        # VGA текстовый режим с hardware cursor
│   ├── serial.rs            # Serial port для тестов
│   ├── qemu.rs              # QEMU exit device
│   ├── interrupts.rs        # CPU exceptions и hardware interrupts (IDT, PIC)
│   ├── gdt.rs               # Global Descriptor Table с TSS
│   ├── memory.rs            # Управление памятью (paging, frame allocator)
│   ├── allocator.rs         # Heap allocator (fixed-size block)
│   ├── power.rs             # Управление питанием (shutdown/reboot via ACPI)
│   ├── shell.rs             # Интерактивный shell с автодополнением и редактированием
│   ├── editor.rs            # Построчный текстовый редактор
│   ├── task/
│   │   ├── mod.rs           # Task infrastructure для async/await
│   │   ├── executor.rs      # Simple task executor
│   │   ├── keyboard.rs      # Async keyboard input stream
│   │   └── timer.rs         # Async timer
│   ├── process/
│   │   ├── mod.rs           # Process management
│   │   ├── context.rs       # Context switching (asm)
│   │   └── scheduler.rs     # Process scheduler (round-robin)
│   ├── drivers/
│   │   ├── mod.rs           # Hardware drivers module
│   │   └── ata.rs           # IDE/ATA PIO disk driver
│   └── fs/
│       ├── mod.rs           # Filesystem module
│       ├── vfs.rs           # VFS - Virtual File System trait и типы
│       ├── ramdisk.rs       # RAM disk - in-memory файловая система
│       └── fat32.rs         # FAT32 filesystem (read-only)
├── tests/
│   ├── basic_boot.rs        # Integration тест загрузки
│   ├── should_panic.rs      # Тест обработки паник
│   ├── heap_allocation.rs   # Тесты heap allocation
│   └── vfs_test.rs          # Тесты VFS и файловой системы
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
- **Stage 12: VFS** ✅ - Virtual File System (абстракция файловой системы)
- **Stage 13: RAM Disk** ✅ - In-memory файловая система (ls, cat, write, rm)
- **Stage 14: FAT32** ✅ - Чтение FAT32 с диска (IDE/ATA PIO driver, boot sector, directory parsing)
- **Stage 15: Context Switching** ✅ - Переключение между процессами
- **Stage 16: Process Scheduler** ✅ - Планировщик процессов (round-robin, preemptive)
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
- **Hardware cursor positioning:**
  - VGA использует 16-битную абсолютную адресацию курсора
  - Позиция = (row × 80) + column для стандартного 80×25 режима
  - Управление через порты 0x3D4 (Command Register) и 0x3D5 (Data Register)
  - Cursor Location High Register (14) и Low Register (15)
  - Методы: set_cursor_column(), get_cursor_column(), move_cursor_left/right()
- Поддержка Backspace и расширенных символов
- Безопасная абстракция поверх небезопасных операций
- **Абсолютное vs относительное позиционирование:**
  - Абсолютное (set_cursor_column) - гарантирует точную позицию, предотвращает glitches
  - Относительное (move_cursor_left/right) - может привести к выходу за границы при многократных вызовах
  - В RustOS используется абсолютное позиционирование для всех операций редактирования

### Управление питанием (Stage 8)

**Shutdown и Reboot через ACPI:**
- Shutdown через ACPI PM1a Control Block (запись в порт 0x604)
- Reboot через PS/2 контроллер (порт 0x64, команда 0xFE)
- Triple fault fallback если ACPI недоступен
- Безопасное завершение работы ядра

### Интерактивный Shell (Stages 9-11)

**Командная оболочка с расширенным функционалом:**
- Буфер ввода с полноценным редактированием
- Навигация курсора: стрелки влево/вправо, Home, End
- Вставка символов в любую позицию курсора
- Delete для удаления символа под курсором
- История команд (до 50 команд) с навигацией стрелками UP/DOWN
- Tab-автодополнение команд и имён файлов (показывает все совпадения при нескольких вариантах)
- Защита от удаления приглашения (backspace блокируется на пустом буфере)
- Фильтрация escape-последовательностей (только printable ASCII)
- **Абсолютное позиционирование курсора** - предотвращает visual glitches при редактировании
  - Курсор всегда устанавливается через set_cursor_column(2 + position)
  - Column 0-1: приглашение "> ", Column 2+: пользовательский ввод
  - Защита от "уезжания" курсора за границы буфера
- Системные команды: help, clear, echo, version, uptime, time, meminfo, history, shutdown, reboot
- Команды управления процессами: ps, kill
- Файловые команды: ls, cat, write, rm, touch, cp, mv, head, tail, wc, grep
- Unix-подобные утилиты с поддержкой опций (например, head -n 5 файл, grep -i -n pattern file)
- Упрощённые pipes для связывания команд (cat file | grep pattern, ls | grep pattern, cat file | wc)
- Встроенный текстовый редактор (edit) для построчного редактирования файлов

### Context Switching (Stage 15)

**Переключение между процессами:**
- Context структура для сохранения состояния CPU (регистры, rip, rsp, rflags)
- Сохранение callee-saved регистров (r15, r14, r13, r12, rbx, rbp)
- switch_context() функция на чистом ассемблере (naked function)
- Process структура с PID, состоянием, контекстом и стеком
- ProcessManager для управления процессами
- ProcessState: Ready, Running, Blocked, Terminated
- Основа для кооперативной и вытесняющей многозадачности

### Process Scheduler (Stage 16)

**Планировщик процессов:**
- Round-robin планировщик с временными квантами
- Вытесняющая многозадачность (preemptive multitasking) через timer interrupt
- Очередь готовых процессов (ready queue)
- Функции для управления процессами:
  - spawn() - создание нового процесса
  - yield_cpu() - кооперативная передача управления
  - block() / unblock() - блокировка и разблокировка процессов
  - terminate() / exit() - завершение процессов
- Интеграция с таймером для автоматического переключения
- Команды shell: ps (список процессов), kill (завершить процесс)
- Поддержка многозадачности на уровне ядра

### Файловая система (Stages 12-13)

**VFS - Virtual File System (Stage 12):**
- FileSystem trait для унифицированного интерфейса
- FileInfo структура для метаданных файлов
- VfsError для стандартизованной обработки ошибок
- Методы: read, write, delete, list, exists
- Управление пространством: used_space, total_space, free_space
- Позволяет легко добавлять новые FS (FAT32, ext2 и т.д.)

**RAM Disk - In-Memory файловая система (Stage 13):**
- Максимум 64 файла по 4 КБ каждый (256 КБ всего)
- Имена файлов до 32 символов
- Операции: создание/перезапись (write), чтение (read), удаление (delete), список (list)
- Реализует FileSystem trait из VFS
- Global Mutex-защищенный singleton (RAMDISK)
- Shell команды: ls, cat, write, rm
- Поддержка UTF-8 текста и бинарных данных (hex dump)

**FAT32 - Полная поддержка файловой системы с директориями (Stage 14+):**
- IDE/ATA PIO driver для чтения и записи секторов на диск
- Поддержка primary IDE bus (порты 0x1F0-0x1F7)
- Парсинг FAT32 boot sector (BPB и Extended Boot Record)
- Чтение и запись FAT таблицы и traversal цепочек кластеров
- Парсинг и создание directory entries (short file names)
- **Полная поддержка вложенных директорий:**
  - Навигация по путям (абсолютные и относительные)
  - Создание директорий (mkdir) с инициализацией . и .. записей
  - Удаление пустых директорий (rmdir)
  - Список содержимого любой директории (ls)
  - Чтение/запись файлов в любой директории
  - Смена текущей директории (cd) с поддержкой .., ., и /
- Реализует FileSystem trait из VFS + расширенные методы для директорий
- Shell команды: mount/umount, cd, mkdir, rmdir
- Persistent command history - автоматическое сохранение на FAT32
- Текущая директория отслеживается в shell
- Тестирование через QEMU с FAT32 disk images

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
- Интерактивный shell с историей команд и Tab-автодополнением (команды и файлы)
- Persistent command history - история команд сохраняется на FAT32 между перезагрузками
- Virtual File System (VFS) - унифицированный интерфейс для FS
- RAM disk файловая система с Unix-утилитами
- FAT32 filesystem - полная поддержка чтения/записи с вложенными директориями через IDE/ATA PIO driver (mount/umount)
- Disk space reporting - команда df для отображения использования дисков
- Расширенные coreutils (ls, cat, cp, mv, rm, touch, head, tail, wc, write, grep, df, pwd, du, find, tree, less/more, which)
- Полноценная навигация по директориям (cd, mkdir, rmdir) с поддержкой абсолютных и относительных путей
- Система алиасов команд (alias/unalias) для создания пользовательских команд
- Дополнительные утилиты (date, hostname, sleep) вдохновленные OpenComputers mod
- Упрощённые pipes для связывания команд (cat | grep, ls | grep, cat | wc)
- Context switching - переключение между процессами (основа для многозадачности)
- Process Scheduler - round-robin планировщик с вытесняющей многозадачностью
- Управление процессами - команды ps и kill для мониторинга и управления процессами
- Комплексная система тестирования (VFS, heap, integration, panic tests)

## Работа с файловой системой

### Автоматическое монтирование FAT32

RustOS **автоматически пытается смонтировать FAT32** при загрузке:

- ✅ **Если FAT32 диск найден** → монтируется автоматически
  - Все файлы и история команд сохраняются между перезагрузками
  - Текущая директория работает с FAT32
  - `ls`, `cd`, `mkdir` и другие команды работают с диском

- ⚠️ **Если FAT32 не найден** → используется RAM disk
  - Данные теряются при перезагрузке
  - Можно смонтировать вручную: `mount fat32`
  - Полезно для тестирования без диска

### Создание FAT32 диска для QEMU

Чтобы данные сохранялись между перезагрузками, создайте виртуальный диск:

```bash
# 1. Создать образ диска 100MB
dd if=/dev/zero of=disk.img bs=1M count=100

# 2. Отформатировать в FAT32
mkfs.fat -F 32 disk.img

# 3. Запустить QEMU с двумя дисками
qemu-system-x86_64 \
    -drive format=raw,file=target/x86_64-rustos/release/bootimage-rustos.bin \
    -drive format=raw,file=disk.img \
    -serial stdio
```

**Что произойдет при загрузке:**
1. ✓ RustOS автоматически найдет FAT32 на втором диске
2. ✓ Смонтирует его и загрузит `.history`
3. ✓ Все созданные файлы сохранятся на диск
4. ✓ При следующей загрузке все файлы будут на месте!

### Пример работы с постоянным хранилищем

```bash
# После автомонтирования FAT32:
> pwd
/

> mkdir projects
Directory 'projects' created

> cd projects
Changed directory to: /projects

> write hello.txt "Hello from persistent storage!"
> cat hello.txt
Hello from persistent storage!

> reboot

# После перезагрузки:
> ls
/ (2 items):
  projects - <DIR>
  .history - 125 bytes

> cd projects
> ls
/projects (1 items):
  hello.txt - 32 bytes

> cat hello.txt
Hello from persistent storage!
```

### Структура файловой системы

```
/ (FAT32 - постоянное хранилище)
├── .history          # История команд (автосохранение)
├── projects/         # Ваши проекты
│   └── rustos/
│       ├── src/
│       │   └── main.rs
│       └── docs/
│           └── readme.txt
├── data/             # Любые данные
└── config/           # Конфигурация
```

### Планируется 📋

- Расширенные pipes с перенаправлением ввода/вывода (>, <, 2>)
- Пользовательское пространство (user mode processes в ring 3)
- ACPI расширенная поддержка (обнаружение устройств)
- Сетевой стек (TCP/IP)
- Системные вызовы (syscall interface)
- ELF загрузчик (запуск программ)
- Цветной вывод в терминале (ANSI escape sequences)

## Производительность

Текущий размер образа: ~1.1 MB (включая bootloader)

## Лицензия

Этот проект создан в образовательных целях.
