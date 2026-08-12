//! Вывод: данные — в stdout, ход работы — в stderr.
//!
//! Разделение не косметическое. В stdout идёт только то, что осмысленно
//! передать дальше по трубе (список треков), поэтому `list-favorites | grep`
//! получает треки, а не пополам с прогрессом. Всё остальное — в stderr.
//!
//! Второе: `println!` при закрытой трубе **паникует**
//! (`failed printing to stdout: Broken pipe`), а `list-favorites | head` —
//! штатный способ пользоваться выводом. Здесь закрытая труба обрабатывается
//! как обычное завершение.

use std::io::{ErrorKind, Write as _, stderr, stdout};

use anyhow::Context as _;

/// Состояние трубы после записи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub(crate) enum Pipe {
    Open,
    /// Читатель ушёл (`| head`): продолжать печатать некому.
    Closed,
}

/// Печатает строку данных в stdout.
///
/// # Errors
///
/// Ошибка при любом отказе записи, кроме закрытой трубы: та возвращается как
/// [`Pipe::Closed`], потому что это не сбой, а нормальный конец работы.
pub(crate) fn line(text: &str) -> anyhow::Result<Pipe> {
    match writeln!(stdout().lock(), "{text}") {
        Ok(()) => Ok(Pipe::Open),
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(Pipe::Closed),
        Err(error) => Err(error).context("не удалось записать в stdout"),
    }
}

/// Сообщает о ходе работы в stderr.
///
/// Отказ записи намеренно игнорируется: диагностика не должна иметь права
/// уронить выгрузку, ради которой её и печатают.
pub(crate) fn progress(text: &str) {
    let _ignored_write = writeln!(stderr().lock(), "{text}");
}
