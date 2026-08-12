# Список рецептов
default:
    @just --list

# Линтер. Флага -D warnings нет намеренно: строгость живёт в [lints] манифеста
clippy:
    cargo clippy --all-targets

# Тесты. Аргумент — фильтр по имени: just test displays_track
test filter='':
    cargo test {{ filter }}

# Известные уязвимости в зависимостях. Требует cargo install cargo-audit
audit:
    cargo audit
