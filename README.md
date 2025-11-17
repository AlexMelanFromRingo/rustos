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
# Обычная сборка
cargo build

# Создание загружаемого образа
cargo bootimage
```

## Запуск

### С помощью QEMU

```bash
# Запуск через cargo run (использует настройки из .cargo/config.toml)
cargo run

# Или напрямую с помощью QEMU
qemu-system-x86_64 -drive format=raw,file=target/x86_64-rustos/debug/bootimage-rustos.bin
```

### Запуск на настоящем оборудовании

**ВНИМАНИЕ:** Это перезапишет данные на USB-накопителе!

```bash
# Создать загружаемый образ
cargo bootimage

# Записать на USB (замените /dev/sdX на ваше устройство)
sudo dd if=target/x86_64-rustos/debug/bootimage-rustos.bin of=/dev/sdX && sync
```

## Структура проекта

```
rustos/
├── src/
│   ├── main.rs              # Точка входа ядра
│   └── vga_buffer.rs        # VGA текстовый режим
├── .cargo/
│   └── config.toml          # Настройки сборки Cargo
├── x86_64-rustos.json       # Спецификация целевой платформы
├── Cargo.toml               # Манифест проекта
├── rust-toolchain           # Указывает использовать nightly
└── README.md                # Этот файл
```

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

## Следующие шаги

После создания минимального ядра, можно добавить:
- Обработку прерываний (IDT)
- Управление памятью (страничная адресация, кучу)
- Многозадачность
- Файловую систему
- Системные вызовы
- И многое другое!

## Лицензия

Этот проект создан в образовательных целях.
