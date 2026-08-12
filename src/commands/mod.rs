//! Команды: сценарии целиком, от запроса к API до результата.
//!
//! Модуль на команду CLI, `main` остаётся тонкой оболочкой — собирает клиент и
//! диспетчеризует. Обращаться к `account`/`library` из `main` не нужно.

pub(crate) mod batch;
pub(crate) mod download_favorite;
pub(crate) mod download_link;
pub(crate) mod download_track;
pub(crate) mod list_favorite;
