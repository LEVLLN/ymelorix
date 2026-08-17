//! Теги в скачиваемых файлах — без сторонних крейтов.
//!
//! Музыка отдаёт файлы вообще без метаданных: в `MP4` от неё нет ни `udta`, ни
//! `meta`, ни `ilst`, поэтому плеер показывает имя файла. Здесь заголовок
//! переписывается на лету, пока тело ещё качается.
//!
//! Всё в этом модуле — чистые функции над байтами: ни сети, ни файлов, ни
//! часов. Отсюда и способ проверки — таблицы в тестах рядом с кодом.

mod flac;
mod id3;
mod mp4;

use core::fmt;

/// Контейнер, в который умеем писать теги.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    /// `MP4`: `aac`, `he-aac` и `flac` в контейнере `MP4` — всё, что уезжает в `.m4a`.
    Mp4,
    /// Голый `FLAC` с блоками метаданных в начале файла.
    Flac,
    /// `MP3` с тегом `ID3v2` перед первым кадром.
    Mp3,
}

impl Format {
    /// Формат по расширению, которое выбрано по кодеку из ответа Музыки.
    ///
    /// `None` — расширение незнакомое: файл записывается как пришёл.
    pub(crate) fn of(extension: &str) -> Option<Self> {
        match extension {
            "m4a" => Some(Self::Mp4),
            "flac" => Some(Self::Flac),
            "mp3" => Some(Self::Mp3),
            _unknown => None,
        }
    }
}

/// Номер трека в альбоме.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Number {
    pub(crate) index: u16,
    pub(crate) total: Option<u16>,
}

/// Что писать в теги.
///
/// Все поля необязательны намеренно: отсутствующее поле не пишется вовсе.
/// Заглушки вроде `<неизвестный исполнитель>` уместны в имени файла, но в теге
/// они хуже пустоты — плеер покажет их как настоящее имя.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Meta {
    pub(crate) title: Option<String>,
    pub(crate) artists: Vec<String>,
    pub(crate) album: Option<String>,
    pub(crate) album_artists: Vec<String>,
    pub(crate) year: Option<u16>,
    pub(crate) genre: Option<String>,
    pub(crate) number: Option<Number>,
    pub(crate) volume: Option<u16>,
}

impl Meta {
    /// Нечего писать: пустой `ilst` хуже отсутствующего.
    fn is_empty(&self) -> bool {
        // Разбор ради проверки на полноту: новое поле сломает компиляцию здесь,
        // а не тихо перестанет учитываться.
        let Self {
            title,
            artists,
            album,
            album_artists,
            year,
            genre,
            number,
            volume,
        } = self;

        title.is_none()
            && artists.is_empty()
            && album.is_none()
            && album_artists.is_empty()
            && year.is_none()
            && genre.is_none()
            && number.is_none()
            && volume.is_none()
    }

    /// Исполнители одной строкой — или ничего, если их нет.
    fn artists(&self) -> Option<String> {
        joined(&self.artists)
    }

    /// Исполнители альбома одной строкой.
    fn album_artists(&self) -> Option<String> {
        joined(&self.album_artists)
    }
}

fn joined(values: &[String]) -> Option<String> {
    match values {
        [] => None,
        present => Some(present.join(", ")),
    }
}

/// Почему теги не записаны.
///
/// Отдельный тип, а не строка: причина уходит в лог полем, и по ней видно,
/// нужен ли разбор полётов или файл просто такой.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reason {
    /// Писать нечего: Музыка не дала ни названия, ни исполнителя.
    Empty,
    /// Структура файла не разобралась — тегировать вслепую опаснее, чем не тегировать.
    Malformed,
    /// Фрагментированный `MP4`: положения фрагментов записаны в индексах,
    /// которых в заголовке не видно, и сдвиг `moov` их обесценит.
    Fragmented,
    /// Отсчёты звука лежат внутри `moov` — сдвинуть его, не разобрав их, нельзя.
    Inline,
    /// Заголовок не уместился в отведённый буфер.
    Oversized,
    /// Длина не помещается в поле формата.
    Overflow,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "нет данных для тегов",
            Self::Malformed => "структура файла не разобрана",
            Self::Fragmented => "фрагментированный MP4",
            Self::Inline => "звук лежит внутри заголовка",
            Self::Oversized => "заголовок не уместился в буфер",
            Self::Overflow => "длина не помещается в поле формата",
        })
    }
}

/// Что делать с накопленным началом файла.
#[derive(Debug)]
pub(crate) enum Patch {
    /// Заголовок ещё не целиком — нужно дочитать.
    More,
    /// Готовое начало файла: заменяет первые `used` байт потока.
    Ready { head: Vec<u8>, used: usize },
    /// Тегировать не будем — писать как пришло.
    Refused(Reason),
}

/// Внутренний результат разборщика: у него нет варианта отказа, отказ — это `Err`.
enum Outcome {
    More,
    Ready { head: Vec<u8>, used: usize },
}

/// Переписывает начало файла, добавив теги.
///
/// Вызывается по мере накопления байт: пока разборщику не хватает данных,
/// возвращается [`Patch::More`]. Результат проверяется обратным разбором —
/// испорченный заголовок не должен доехать до диска.
pub(crate) fn patch(format: Format, head: &[u8], meta: &Meta) -> Patch {
    if meta.is_empty() {
        return Patch::Refused(Reason::Empty);
    }

    let built = match format {
        Format::Mp4 => mp4::patch(head, meta),
        Format::Flac => flac::patch(head, meta),
        Format::Mp3 => id3::patch(head, meta),
    };

    match built {
        Err(reason) => Patch::Refused(reason),
        Ok(Outcome::More) => Patch::More,
        Ok(Outcome::Ready { head, used }) => match verify(format, &head) {
            Ok(()) => Patch::Ready { head, used },
            Err(reason) => Patch::Refused(reason),
        },
    }
}

/// Разбирает собранный заголовок обратно.
///
/// Дешёвая проверка: заголовок ещё в памяти, а цена ошибки — файл, который
/// откроется и не проиграется.
fn verify(format: Format, head: &[u8]) -> Result<(), Reason> {
    match format {
        Format::Mp4 => mp4::verify(head),
        Format::Flac => flac::verify(head),
        Format::Mp3 => id3::verify(head),
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::{Format, Meta, Number, Patch, Reason};

    /// Исполнитель, альбом и трек выдуманы: тест не должен зависеть от того,
    /// что происходит с чужими каталогами.
    pub(super) fn sample() -> Meta {
        Meta {
            title: Some("Сорок седьмой этаж".to_owned()),
            artists: vec!["Ветер Овна".to_owned()],
            album: Some("Пыль на антресолях".to_owned()),
            album_artists: vec!["Ветер Овна".to_owned()],
            year: Some(2018),
            genre: Some("rap".to_owned()),
            number: Some(Number {
                index: 3,
                total: Some(12),
            }),
            volume: Some(1),
        }
    }

    #[rstest]
    #[case::m4a("m4a", Some(Format::Mp4))]
    #[case::flac("flac", Some(Format::Flac))]
    #[case::mp3("mp3", Some(Format::Mp3))]
    #[case::unknown("opus", None)]
    fn maps_extension_to_format(#[case] extension: &str, #[case] expected: Option<Format>) {
        assert_eq!(Format::of(extension), expected);
    }

    /// Лучше файл без тегов, чем файл с пустым `ilst`: второй ещё и выглядит
    /// так, будто теги записаны.
    #[rstest]
    #[case::mp4(Format::Mp4)]
    #[case::flac(Format::Flac)]
    #[case::mp3(Format::Mp3)]
    fn refuses_to_write_nothing(#[case] format: Format) {
        assert!(matches!(
            super::patch(format, &[], &Meta::default()),
            Patch::Refused(Reason::Empty)
        ));
    }

    #[test]
    fn joins_artists_and_keeps_absence_absent() {
        let meta = sample();
        assert_eq!(meta.artists(), Some("Ветер Овна".to_owned()));
        assert_eq!(Meta::default().artists(), None);
    }
}
