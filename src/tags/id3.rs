//! Теги `MP3`: `ID3v2.4` перед первым кадром.
//!
//! Самый простой из трёх случаев: тег просто лежит в начале файла, ничего не
//! сдвигая. Единственная тонкость — длины пишутся «синхробезопасно», по семь
//! значащих бит в байте, чтобы в них не встретилась последовательность,
//! похожая на заголовок кадра.

use super::{Meta, Outcome, Reason};

const MAGIC: &[u8; 3] = b"ID3";

/// Заголовок тега: сигнатура (3) + версия (2) + флаги (1) + длина (4).
const HEADER: usize = 10;

/// Флаг в заголовке: у тега есть завершающая копия заголовка.
const FOOTER: u8 = 0x10;

/// Кодировка текста в кадре: `UTF-8`.
const UTF8: u8 = 0x03;

/// Потолок синхробезопасного числа: четыре байта по семь бит.
const MAX_SIZE: u32 = 0x0FFF_FFFF;

/// Дописывает тег в начало файла, отбросив прежний.
///
/// # Errors
///
/// [`Reason::Malformed`] на нечитаемой длине прежнего тега,
/// [`Reason::Overflow`], если свой тег не влезает в синхробезопасную длину.
pub(super) fn patch(head: &[u8], meta: &Meta) -> Result<Outcome, Reason> {
    let Some(used) = existing(head)? else {
        return Ok(Outcome::More);
    };

    Ok(Outcome::Ready {
        head: tag(meta)?,
        used,
    })
}

/// Проверяет собранный тег обратным разбором.
///
/// # Errors
///
/// [`Reason::Malformed`], если длина в заголовке не сходится с длиной кадров.
pub(super) fn verify(head: &[u8]) -> Result<(), Reason> {
    match existing(head)? {
        Some(len) if len == head.len() => Ok(()),
        _other => Err(Reason::Malformed),
    }
}

/// Длина прежнего тега — или `0`, если его нет.
///
/// `None` — тег есть, но дочитан не целиком: пропустить можно только те байты,
/// которые уже пришли.
fn existing(head: &[u8]) -> Result<Option<usize>, Reason> {
    match head.get(..MAGIC.len()) {
        None => return Ok(None),
        Some(magic) if magic != MAGIC => return Ok(Some(0)),
        Some(_found) => {}
    }

    let Some(header) = head.get(..HEADER) else {
        return Ok(None);
    };
    let flags = header.get(5).copied().ok_or(Reason::Malformed)?;
    let size = usize::try_from(decoded(header.get(6..HEADER).ok_or(Reason::Malformed)?)?)
        .map_err(|_overflow| Reason::Overflow)?;

    let footer = if flags & FOOTER == 0 { 0 } else { HEADER };
    let total = size
        .checked_add(HEADER)
        .and_then(|len| len.checked_add(footer))
        .ok_or(Reason::Overflow)?;

    Ok((head.len() >= total).then_some(total))
}

fn tag(meta: &Meta) -> Result<Vec<u8>, Reason> {
    let frames = frames(meta)?;
    let size = u32::try_from(frames.len()).map_err(|_overflow| Reason::Overflow)?;

    Ok([
        &MAGIC[..],
        &[0x04, 0x00, 0x00],
        &encoded(size)?,
        frames.as_slice(),
    ]
    .concat())
}

fn frames(meta: &Meta) -> Result<Vec<u8>, Reason> {
    let mut frames: Vec<Vec<u8>> = Vec::new();

    if let Some(title) = &meta.title {
        frames.push(frame(*b"TIT2", title)?);
    }
    if let Some(artists) = meta.artists() {
        frames.push(frame(*b"TPE1", &artists)?);
    }
    if let Some(album) = &meta.album {
        frames.push(frame(*b"TALB", album)?);
    }
    if let Some(artists) = meta.album_artists() {
        frames.push(frame(*b"TPE2", &artists)?);
    }
    if let Some(year) = meta.year {
        frames.push(frame(*b"TDRC", &year.to_string())?);
    }
    if let Some(genre) = &meta.genre {
        frames.push(frame(*b"TCON", genre)?);
    }
    if let Some(number) = meta.number {
        let value = match number.total {
            Some(total) => format!("{}/{total}", number.index),
            None => number.index.to_string(),
        };
        frames.push(frame(*b"TRCK", &value)?);
    }
    if let Some(volume) = meta.volume {
        frames.push(frame(*b"TPOS", &volume.to_string())?);
    }

    Ok(frames.concat())
}

fn frame(id: [u8; 4], value: &str) -> Result<Vec<u8>, Reason> {
    let body = [&[UTF8][..], value.as_bytes()].concat();
    let size = u32::try_from(body.len()).map_err(|_overflow| Reason::Overflow)?;

    Ok([id.as_slice(), &encoded(size)?, &[0, 0], body.as_slice()].concat())
}

/// Синхробезопасная запись: по семь значащих бит в байте.
fn encoded(value: u32) -> Result<[u8; 4], Reason> {
    if value > MAX_SIZE {
        return Err(Reason::Overflow);
    }

    let mut out = [0_u8; 4];
    for (index, byte) in out.iter_mut().enumerate() {
        let shift = 21_u32.saturating_sub(u32::try_from(index).unwrap_or(0).saturating_mul(7));
        *byte = u8::try_from((value >> shift) & 0x7F).map_err(|_overflow| Reason::Overflow)?;
    }

    Ok(out)
}

fn decoded(bytes: &[u8]) -> Result<u32, Reason> {
    bytes.iter().try_fold(0_u32, |value, byte| {
        // Бит 0x80 в длине означает, что запись не синхробезопасна: читать её
        // как обычное число — верный способ отрезать не там.
        if byte & 0x80 != 0 {
            return Err(Reason::Malformed);
        }
        value
            .checked_mul(128)
            .and_then(|shifted| shifted.checked_add(u32::from(*byte)))
            .ok_or(Reason::Overflow)
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use rstest::rstest;

    use super::super::{Meta, Outcome, tests::sample};
    use super::{HEADER, MAGIC, decoded, encoded, patch, verify};

    fn ready(bytes: &[u8], meta: &Meta) -> Option<(Vec<u8>, usize)> {
        match patch(bytes, meta) {
            Ok(Outcome::Ready { head, used }) => Some((head, used)),
            _other => None,
        }
    }

    #[test]
    fn prepends_a_tag_to_a_bare_file() {
        let (head, used) = ready(&[0xFF, 0xFB, 0x90, 0x00], &sample()).unwrap();

        assert_eq!(used, 0);
        assert!(verify(&head).is_ok());
        assert_eq!(head.get(..3), Some(&MAGIC[..]));
    }

    #[test]
    fn writes_every_field_it_was_given() {
        let (head, _used) = ready(&[0xFF, 0xFB, 0x90, 0x00], &sample()).unwrap();
        let text = String::from_utf8_lossy(&head).into_owned();

        for expected in [
            "TIT2", "TPE1", "TALB", "TPE2", "TDRC", "TCON", "TRCK", "TPOS",
        ] {
            assert!(text.contains(expected), "нет кадра {expected}");
        }
        assert!(text.contains("Сорок седьмой этаж"));
        assert!(text.contains("3/12"));
    }

    /// Прежний тег отбрасывается целиком: два `ID3` подряд читает не всякий плеер.
    #[test]
    fn drops_an_existing_tag() {
        let old = [
            &MAGIC[..],
            &[0x03, 0x00, 0x00],
            &encoded(6).unwrap(),
            &[0; 6],
        ]
        .concat();
        let file = [old.as_slice(), &[0xFF, 0xFB]].concat();

        let (head, used) = ready(&file, &sample()).unwrap();
        assert_eq!(used, HEADER + 6);
        assert!(verify(&head).is_ok());
    }

    #[test]
    fn asks_for_more_until_the_old_tag_is_whole() {
        let truncated = [&MAGIC[..], &[0x03, 0x00, 0x00], &encoded(64).unwrap()].concat();

        assert!(matches!(patch(&truncated, &sample()), Ok(Outcome::More)));
    }

    #[rstest]
    #[case::zero(0)]
    #[case::small(127)]
    #[case::carries(128)]
    #[case::large(0x0FFF_FFFF)]
    fn round_trips_synchsafe_lengths(#[case] value: u32) {
        assert_eq!(decoded(&encoded(value).unwrap()), Ok(value));
    }
}
