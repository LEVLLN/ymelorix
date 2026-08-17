//! Теги `MP4`: `moov > udta > meta > ilst`.
//!
//! Главная сложность не в самих атомах, а в том, что `stco` хранит **абсолютные**
//! смещения отсчётов в файле, а `moov` у файлов Музыки лежит **до** `mdat`.
//! Вставка `udta` сдвигает весь звук, и каждое смещение обязано сдвинуться
//! вместе с ним — иначе файл откроется и не проиграется.

use super::{Meta, Outcome, Reason};

/// Размер (4) + тип (4).
const HEADER: usize = 8;

/// Заголовок бокса с 64-битной длиной: размер (4) + тип (4) + длина (8).
const LARGE_HEADER: usize = 16;

/// Контейнеры, внутрь которых имеет смысл спускаться в поисках `stco`.
const CONTAINERS: [&[u8; 4]; 5] = [b"moov", b"trak", b"mdia", b"minf", b"stbl"];

/// Обработчик метаданных iTunes: `meta` без него читают не все плееры.
const HANDLER: [u8; 33] = [
    0x00, 0x00, 0x00, 0x21, b'h', b'd', b'l', b'r', // размер и тип
    0x00, 0x00, 0x00, 0x00, // версия и флаги
    0x00, 0x00, 0x00, 0x00, // pre_defined
    b'm', b'd', b'i', b'r', // тип обработчика
    b'a', b'p', b'p', b'l', // reserved, как его пишет iTunes
    0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, //
    0x00, // пустое имя
];

const NAME: [u8; 4] = [0xA9, b'n', b'a', b'm'];
const ARTIST: [u8; 4] = [0xA9, b'A', b'R', b'T'];
const ALBUM: [u8; 4] = [0xA9, b'a', b'l', b'b'];
const ALBUM_ARTIST: [u8; 4] = *b"aART";
const YEAR: [u8; 4] = [0xA9, b'd', b'a', b'y'];
const GENRE: [u8; 4] = [0xA9, b'g', b'e', b'n'];
const TRACK: [u8; 4] = *b"trkn";
const VOLUME: [u8; 4] = *b"disk";

/// Тип полезной нагрузки в боксе `data`: текст в `UTF-8`.
const TEXT: u32 = 1;
/// Тип полезной нагрузки в боксе `data`: байты без интерпретации.
const BINARY: u32 = 0;

/// Границы бокса в разбираемом срезе.
#[derive(Debug, Clone, Copy)]
struct Span {
    kind: [u8; 4],
    start: usize,
    header: usize,
    size: usize,
}

impl Span {
    fn end(&self) -> usize {
        self.start.saturating_add(self.size)
    }

    fn payload<'a>(&self, bytes: &'a [u8]) -> Result<&'a [u8], Reason> {
        let from = self
            .start
            .checked_add(self.header)
            .ok_or(Reason::Malformed)?;
        bytes.get(from..self.end()).ok_or(Reason::Malformed)
    }

    fn whole<'a>(&self, bytes: &'a [u8]) -> Result<&'a [u8], Reason> {
        bytes.get(self.start..self.end()).ok_or(Reason::Malformed)
    }
}

/// Абсолютные границы прежнего `moov` в файле.
struct Bounds {
    start: u64,
    end: u64,
}

/// Таблица смещений: где лежит её тело и по сколько байт в записи.
struct Table {
    at: usize,
    width: usize,
}

/// Дописывает теги в начало файла.
///
/// # Errors
///
/// [`Reason::Malformed`] на неразобранной структуре, [`Reason::Fragmented`] на
/// фрагментированном файле, [`Reason::Inline`], если отсчёты лежат внутри `moov`.
pub(super) fn patch(head: &[u8], meta: &Meta) -> Result<Outcome, Reason> {
    let top = spans(head)?;
    let Some(moov) = top.iter().copied().find(|span| span.kind == *b"moov") else {
        return Ok(Outcome::More);
    };

    let payload = moov.payload(head)?;
    let children = spans(payload)?;
    if children.last().map_or(0, Span::end) != payload.len() {
        return Err(Reason::Malformed);
    }
    // `mvex` объявляет файл фрагментированным: положения фрагментов записаны в
    // `sidx` и `mfra`, которые лежат за пределами заголовка и после сдвига
    // укажут мимо.
    if children.iter().any(|span| span.kind == *b"mvex") {
        return Err(Reason::Fragmented);
    }

    let rebuilt = rebuild(payload, &children, meta)?;
    let delta = i64::try_from(rebuilt.len())
        .and_then(|new| i64::try_from(moov.size).map(|old| new.saturating_sub(old)))
        .map_err(|_overflow| Reason::Overflow)?;

    let bounds = Bounds {
        start: u64::try_from(moov.start).map_err(|_overflow| Reason::Overflow)?,
        end: u64::try_from(moov.end()).map_err(|_overflow| Reason::Overflow)?,
    };
    let rebuilt = shift(rebuilt, &bounds, delta)?;

    let before = head.get(..moov.start).ok_or(Reason::Malformed)?;
    Ok(Outcome::Ready {
        head: [before, rebuilt.as_slice()].concat(),
        used: moov.end(),
    })
}

/// Проверяет собранный заголовок обратным разбором.
///
/// # Errors
///
/// [`Reason::Malformed`], если боксы не смыкаются или `udta` не на месте.
pub(super) fn verify(head: &[u8]) -> Result<(), Reason> {
    let top = spans(head)?;
    if top.last().map_or(0, Span::end) != head.len() {
        return Err(Reason::Malformed);
    }

    let moov = top
        .iter()
        .find(|span| span.kind == *b"moov")
        .ok_or(Reason::Malformed)?;
    let payload = moov.payload(head)?;
    let children = spans(payload)?;
    if children.last().map_or(0, Span::end) != payload.len() {
        return Err(Reason::Malformed);
    }

    match children.iter().filter(|span| span.kind == *b"udta").count() {
        1 => Ok(()),
        _wrong => Err(Reason::Malformed),
    }
}

/// Пересобирает `moov`: прежние дети, кроме `udta`, плюс свой `udta`.
///
/// Прежний `udta` именно **заменяется**: два `udta` в одном `moov` — это уже
/// испорченный файл, а повторный проход по размеченному файлу возможен.
fn rebuild(payload: &[u8], children: &[Span], meta: &Meta) -> Result<Vec<u8>, Reason> {
    let kept = children
        .iter()
        .filter(|span| span.kind != *b"udta")
        .map(|span| span.whole(payload))
        .collect::<Result<Vec<_>, Reason>>()?
        .concat();

    boxed(*b"moov", &[kept, udta(meta)?].concat())
}

/// Сдвигает смещения отсчётов на длину вставки.
///
/// Правило одно и покрывает обе раскладки файла: смещение до `moov` не
/// двигается, смещение после `moov` двигается на `delta`, а смещение **внутрь**
/// `moov` означает, что звук лежит в заголовке, — такой файл не трогаем.
fn shift(mut moov: Vec<u8>, bounds: &Bounds, delta: i64) -> Result<Vec<u8>, Reason> {
    let mut tables = Vec::new();
    collect(
        moov.get(HEADER..).ok_or(Reason::Malformed)?,
        HEADER,
        &mut tables,
    )?;

    for table in &tables {
        let at = table.at;
        let count = read(&moov, at.checked_add(4).ok_or(Reason::Malformed)?, 4)?;
        let count = usize::try_from(count).map_err(|_overflow| Reason::Overflow)?;

        for index in 0..count {
            let entry = index
                .checked_mul(table.width)
                .and_then(|offset| offset.checked_add(8))
                .and_then(|offset| offset.checked_add(at))
                .ok_or(Reason::Malformed)?;

            let value = moved(read(&moov, entry, table.width)?, bounds, delta)?;
            write(&mut moov, entry, table.width, value)?;
        }
    }

    Ok(moov)
}

fn moved(offset: u64, bounds: &Bounds, delta: i64) -> Result<u64, Reason> {
    match offset {
        before if before < bounds.start => Ok(before),
        inside if inside < bounds.end => Err(Reason::Inline),
        after => after.checked_add_signed(delta).ok_or(Reason::Overflow),
    }
}

/// Собирает `stco` и `co64` со всей глубины `moov`.
fn collect(bytes: &[u8], base: usize, into: &mut Vec<Table>) -> Result<(), Reason> {
    for span in spans(bytes)? {
        let at = base
            .checked_add(span.start)
            .and_then(|start| start.checked_add(span.header))
            .ok_or(Reason::Malformed)?;

        match &span.kind {
            b"stco" => into.push(Table { at, width: 4 }),
            b"co64" => into.push(Table { at, width: 8 }),
            kind if CONTAINERS.contains(&kind) => collect(span.payload(bytes)?, at, into)?,
            _leaf => {}
        }
    }

    Ok(())
}

/// Разбирает подряд идущие боксы, пока они целиком помещаются в срез.
///
/// Обрыв на середине бокса — не ошибка, а «данных пока мало»: разбор
/// останавливается, а вызывающий решает, дочитывать или сдаваться.
fn spans(bytes: &[u8]) -> Result<Vec<Span>, Reason> {
    let mut found = Vec::new();
    let mut at = 0_usize;

    while let Some(rest) = bytes.get(at..) {
        if rest.len() < HEADER {
            break;
        }

        let declared = read(rest, 0, 4)?;
        let kind: [u8; 4] = rest
            .get(4..HEADER)
            .and_then(|slice| <[u8; 4]>::try_from(slice).ok())
            .ok_or(Reason::Malformed)?;

        let (size, header) = match declared {
            // «До конца файла»: длина неизвестна, дальше разбирать нечего.
            0 => break,
            1 if rest.len() < LARGE_HEADER => break,
            1 => (
                usize::try_from(read(rest, HEADER, 8)?).map_err(|_overflow| Reason::Overflow)?,
                LARGE_HEADER,
            ),
            small if small < 8 => return Err(Reason::Malformed),
            other => (
                usize::try_from(other).map_err(|_overflow| Reason::Overflow)?,
                HEADER,
            ),
        };

        if size < header {
            return Err(Reason::Malformed);
        }
        if rest.len() < size {
            break;
        }

        found.push(Span {
            kind,
            start: at,
            header,
            size,
        });
        at = at.checked_add(size).ok_or(Reason::Malformed)?;
    }

    Ok(found)
}

fn read(bytes: &[u8], at: usize, width: usize) -> Result<u64, Reason> {
    let to = at.checked_add(width).ok_or(Reason::Malformed)?;
    bytes
        .get(at..to)
        .ok_or(Reason::Malformed)?
        .iter()
        .try_fold(0_u64, |value, byte| {
            value
                .checked_mul(256)
                .and_then(|shifted| shifted.checked_add(u64::from(*byte)))
                .ok_or(Reason::Overflow)
        })
}

fn write(bytes: &mut [u8], at: usize, width: usize, value: u64) -> Result<(), Reason> {
    // Смещение, не влезшее в четыре байта, требует перестройки `stco` в `co64`:
    // это уже не «дописать тег», и делать это молча нельзя.
    if width == 4 && value > u64::from(u32::MAX) {
        return Err(Reason::Overflow);
    }

    let to = at.checked_add(width).ok_or(Reason::Malformed)?;
    let full = value.to_be_bytes();
    let source = full
        .get(full.len().checked_sub(width).ok_or(Reason::Overflow)?..)
        .ok_or(Reason::Overflow)?;

    bytes
        .get_mut(at..to)
        .ok_or(Reason::Malformed)?
        .copy_from_slice(source);
    Ok(())
}

fn boxed(kind: [u8; 4], payload: &[u8]) -> Result<Vec<u8>, Reason> {
    let size = payload
        .len()
        .checked_add(HEADER)
        .ok_or(Reason::Overflow)
        .and_then(|size| u32::try_from(size).map_err(|_overflow| Reason::Overflow))?;

    Ok([size.to_be_bytes().as_slice(), kind.as_slice(), payload].concat())
}

fn udta(meta: &Meta) -> Result<Vec<u8>, Reason> {
    let items = boxed(*b"ilst", &ilst(meta)?)?;
    let body = [&[0, 0, 0, 0][..], &HANDLER, &items].concat();

    boxed(*b"udta", &boxed(*b"meta", &body)?)
}

fn ilst(meta: &Meta) -> Result<Vec<u8>, Reason> {
    let mut items: Vec<Vec<u8>> = Vec::new();

    if let Some(title) = &meta.title {
        items.push(text(NAME, title)?);
    }
    if let Some(artists) = meta.artists() {
        items.push(text(ARTIST, &artists)?);
    }
    if let Some(album) = &meta.album {
        items.push(text(ALBUM, album)?);
    }
    if let Some(artists) = meta.album_artists() {
        items.push(text(ALBUM_ARTIST, &artists)?);
    }
    if let Some(year) = meta.year {
        items.push(text(YEAR, &year.to_string())?);
    }
    if let Some(genre) = &meta.genre {
        items.push(text(GENRE, genre)?);
    }
    if let Some(number) = meta.number {
        let index = number.index.to_be_bytes();
        let total = number.total.unwrap_or(0).to_be_bytes();
        let payload = [0, 0, index[0], index[1], total[0], total[1], 0, 0];
        items.push(boxed(TRACK, &data(BINARY, &payload)?)?);
    }
    if let Some(volume) = meta.volume {
        let index = volume.to_be_bytes();
        let payload = [0, 0, index[0], index[1], 0, 0];
        items.push(boxed(VOLUME, &data(BINARY, &payload)?)?);
    }

    Ok(items.concat())
}

fn text(kind: [u8; 4], value: &str) -> Result<Vec<u8>, Reason> {
    boxed(kind, &data(TEXT, value.as_bytes())?)
}

fn data(kind: u32, payload: &[u8]) -> Result<Vec<u8>, Reason> {
    boxed(
        *b"data",
        &[&kind.to_be_bytes()[..], &[0, 0, 0, 0], payload].concat(),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use rstest::rstest;

    use super::super::{Meta, Outcome, Reason, tests::sample};
    use super::{HEADER, Span, collect, patch, read, spans, verify};

    /// Тело `mdat` в собранном для тестов файле.
    const SOUND: usize = 64;

    /// Минимальный файл той же раскладки, что отдаёт Музыка: `moov` до `mdat`,
    /// одна таблица `stco` с абсолютными смещениями внутрь `mdat`.
    fn file(offsets: &[u32]) -> Vec<u8> {
        let mut table = vec![0, 0, 0, 0];
        table.extend(u32::try_from(offsets.len()).unwrap().to_be_bytes());
        for offset in offsets {
            table.extend(offset.to_be_bytes());
        }

        let stbl = wrap(*b"stbl", &wrap(*b"stco", &table));
        let moov = wrap(
            *b"moov",
            &wrap(*b"trak", &wrap(*b"mdia", &wrap(*b"minf", &stbl))),
        );

        [
            wrap(*b"ftyp", b"isom\x00\x00\x02\x00"),
            moov,
            wrap(*b"mdat", &[0xAB; SOUND]),
        ]
        .concat()
    }

    fn wrap(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = u32::try_from(payload.len() + HEADER).unwrap();
        [size.to_be_bytes().as_slice(), kind.as_slice(), payload].concat()
    }

    /// Читает `stco` обратно — той же машинерией, что и правит.
    fn offsets(head: &[u8]) -> Vec<u64> {
        let top = spans(head).unwrap();
        let moov = top.iter().find(|span| span.kind == *b"moov").unwrap();
        let mut tables = Vec::new();
        collect(
            moov.payload(head).unwrap(),
            moov.start + moov.header,
            &mut tables,
        )
        .unwrap();

        tables
            .iter()
            .flat_map(|table| {
                let count = usize::try_from(read(head, table.at + 4, 4).unwrap()).unwrap();
                (0..count)
                    .map(|index| {
                        read(head, table.at + 8 + index * table.width, table.width).unwrap()
                    })
                    .collect::<Vec<u64>>()
            })
            .collect()
    }

    fn ready(bytes: &[u8], meta: &Meta) -> Option<(Vec<u8>, usize)> {
        match patch(bytes, meta) {
            Ok(Outcome::Ready { head, used }) => Some((head, used)),
            _other => None,
        }
    }

    /// Склеивает переписанное начало с остатком исходного файла — ровно то, что
    /// делает загрузчик, когда пишет на диск.
    fn spliced(original: &[u8], meta: &Meta) -> Vec<u8> {
        let (head, used) = ready(original, meta).unwrap();
        head.into_iter()
            .chain(original.iter().copied().skip(used))
            .collect()
    }

    #[test]
    fn moves_chunk_offsets_by_the_length_it_inserted() {
        let original = file(&[100, 140]);
        let (head, used) = ready(&original, &sample()).unwrap();

        let delta = u64::try_from(head.len() - used).unwrap();
        assert_eq!(offsets(&head), vec![100 + delta, 140 + delta]);
    }

    /// Свойство, ради которого всё и затевалось: байт, на который указывало
    /// смещение, обязан остаться тем же байтом.
    #[test]
    fn keeps_offsets_pointing_at_the_same_bytes() {
        let target = u32::try_from(file(&[0]).len() - SOUND + 7).unwrap();
        let mut original = file(&[target]);
        *original.get_mut(usize::try_from(target).unwrap()).unwrap() = 0x5A;

        let patched = spliced(&original, &sample());
        let moved = usize::try_from(offsets(&patched).first().copied().unwrap()).unwrap();

        assert_eq!(patched.get(moved).copied(), Some(0x5A));
    }

    #[test]
    fn asks_for_more_until_moov_is_whole() {
        let original = file(&[100]);
        assert!(matches!(
            patch(original.get(..24).unwrap(), &sample()),
            Ok(Outcome::More)
        ));
    }

    #[test]
    fn writes_an_ilst_that_survives_reparsing() {
        let (head, _used) = ready(&file(&[100]), &sample()).unwrap();

        assert!(verify(&head).is_ok());
        assert!(head.windows(4).any(|window| window == b"ilst"));
        assert!(head.windows(4).any(|window| window == b"\xA9nam"));
        assert!(head.windows(4).any(|window| window == b"trkn"));
    }

    /// Повторный проход по уже размеченному файлу обязан заменить `udta`, а не
    /// приложить второй: два `udta` в `moov` — это уже испорченный файл.
    #[test]
    fn replaces_tags_instead_of_stacking_them() {
        let once = spliced(&file(&[100]), &sample());
        let (twice, _used) = ready(&once, &sample()).unwrap();

        assert!(verify(&twice).is_ok());
        assert_eq!(offsets(&twice), offsets(&once));
    }

    /// Отсчёты внутри `moov` встречаются у файлов, собранных нестандартно:
    /// сдвигать такой заголовок нельзя, а притворяться, что всё хорошо, — тем более.
    #[test]
    fn refuses_when_sound_lives_inside_the_header() {
        assert_eq!(patch(&file(&[40]), &sample()).err(), Some(Reason::Inline));
    }

    #[rstest]
    #[case::short_box(vec![0, 0, 0, 3, b'b', b'a', b'd', b'!'])]
    fn refuses_broken_input(#[case] bytes: Vec<u8>) {
        assert_eq!(patch(&bytes, &sample()).err(), Some(Reason::Malformed));
    }

    #[test]
    fn spans_stop_at_a_truncated_box() {
        let found: Vec<Span> = spans(&[0, 0, 0, 16, b'f', b't', b'y', b'p', 0, 0]).unwrap();
        assert!(found.is_empty());
    }
}
