//! Теги голого `FLAC`: блок `VORBIS_COMMENT` среди метаданных.
//!
//! Здесь всё проще, чем в `MP4`: блоки метаданных лежат в начале файла, а
//! `SEEKTABLE` считает смещения **от первого кадра**, а не от начала файла, —
//! вставка блока их не задевает.

use super::{Meta, Outcome, Reason};

const MAGIC: &[u8; 4] = b"fLaC";

/// Заголовок блока: тип с флагом последнего (1) + длина (3).
const HEADER: usize = 4;

/// Потолок длины блока: она записана тремя байтами.
const MAX_BLOCK: usize = 0x00FF_FFFF;

const STREAMINFO: u8 = 0;
const COMMENT: u8 = 4;

/// Кто записал комментарий — обязательное поле формата.
const VENDOR: &str = concat!("ymelorix ", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, Copy)]
struct Block {
    kind: u8,
    start: usize,
    len: usize,
}

impl Block {
    fn end(&self) -> usize {
        self.start.saturating_add(HEADER).saturating_add(self.len)
    }

    fn body<'a>(&self, bytes: &'a [u8]) -> Result<&'a [u8], Reason> {
        let from = self.start.checked_add(HEADER).ok_or(Reason::Malformed)?;
        bytes.get(from..self.end()).ok_or(Reason::Malformed)
    }
}

/// Дописывает теги в начало файла.
///
/// # Errors
///
/// [`Reason::Malformed`], если это не `FLAC` или блоки не разобрались;
/// [`Reason::Overflow`], если комментарий не влезает в трёхбайтовую длину.
pub(super) fn patch(head: &[u8], meta: &Meta) -> Result<Outcome, Reason> {
    let Some(blocks) = blocks(head)? else {
        return Ok(Outcome::More);
    };

    // `STREAMINFO` обязан быть первым блоком — на этом держится порядок сборки.
    match blocks.first() {
        Some(first) if first.kind == STREAMINFO => {}
        _other => return Err(Reason::Malformed),
    }

    let used = blocks.last().map_or(HEADER, Block::end);
    let bodies = rearranged(head, &blocks, meta)?;

    let mut out = MAGIC.to_vec();
    let last = bodies.len().saturating_sub(1);
    for (index, (kind, body)) in bodies.iter().enumerate() {
        emit(*kind, body, index == last, &mut out)?;
    }

    Ok(Outcome::Ready { head: out, used })
}

/// Проверяет собранный заголовок обратным разбором.
///
/// # Errors
///
/// [`Reason::Malformed`], если блоки не смыкаются или комментарий не ровно один.
pub(super) fn verify(head: &[u8]) -> Result<(), Reason> {
    let blocks = blocks(head)?.ok_or(Reason::Malformed)?;
    if blocks.last().map_or(0, Block::end) != head.len() {
        return Err(Reason::Malformed);
    }

    match blocks.iter().filter(|block| block.kind == COMMENT).count() {
        1 => Ok(()),
        _wrong => Err(Reason::Malformed),
    }
}

/// `STREAMINFO`, затем свой комментарий, затем всё прочее без прежнего комментария.
fn rearranged(head: &[u8], blocks: &[Block], meta: &Meta) -> Result<Vec<(u8, Vec<u8>)>, Reason> {
    let kept = blocks
        .iter()
        .filter(|block| block.kind != COMMENT && block.kind != STREAMINFO)
        .map(|block| block.body(head).map(|body| (block.kind, body.to_vec())))
        .collect::<Result<Vec<_>, Reason>>()?;

    let streaminfo = blocks
        .first()
        .ok_or(Reason::Malformed)?
        .body(head)?
        .to_vec();

    Ok([
        vec![(STREAMINFO, streaminfo), (COMMENT, comment(meta))],
        kept,
    ]
    .concat())
}

fn emit(kind: u8, body: &[u8], last: bool, out: &mut Vec<u8>) -> Result<(), Reason> {
    if body.len() > MAX_BLOCK {
        return Err(Reason::Overflow);
    }
    let len = u32::try_from(body.len()).map_err(|_overflow| Reason::Overflow)?;

    out.push(if last { kind | 0x80 } else { kind });
    out.extend(len.to_be_bytes().into_iter().skip(1));
    out.extend_from_slice(body);
    Ok(())
}

/// Разбирает блоки метаданных целиком.
///
/// `None` — последний блок ещё не дочитан.
fn blocks(head: &[u8]) -> Result<Option<Vec<Block>>, Reason> {
    match head.get(..MAGIC.len()) {
        None => return Ok(None),
        Some(magic) if magic != MAGIC => return Err(Reason::Malformed),
        Some(_ok) => {}
    }

    let mut found = Vec::new();
    let mut at = MAGIC.len();

    loop {
        let to = at.checked_add(HEADER).ok_or(Reason::Malformed)?;
        let Some(header) = head.get(at..to) else {
            return Ok(None);
        };

        let flag = header.first().copied().ok_or(Reason::Malformed)?;
        let len = header
            .iter()
            .skip(1)
            .fold(0_usize, |value, byte| value * 256 + usize::from(*byte));

        let block = Block {
            kind: flag & 0x7F,
            start: at,
            len,
        };
        if head.len() < block.end() {
            return Ok(None);
        }

        found.push(block);
        at = block.end();

        if flag & 0x80 != 0 {
            return Ok(Some(found));
        }
    }
}

/// Тело блока `VORBIS_COMMENT`: длины — 32 бита, порядок байт **младшим вперёд**.
fn comment(meta: &Meta) -> Vec<u8> {
    let entries = entries(meta);

    let mut out = Vec::new();
    out.extend(length(VENDOR.len()));
    out.extend_from_slice(VENDOR.as_bytes());
    out.extend(length(entries.len()));
    for entry in &entries {
        out.extend(length(entry.len()));
        out.extend_from_slice(entry.as_bytes());
    }

    out
}

fn entries(meta: &Meta) -> Vec<String> {
    let mut entries = Vec::new();

    if let Some(title) = &meta.title {
        entries.push(format!("TITLE={title}"));
    }
    // Формат разрешает повтор поля, и плееры это понимают лучше склейки:
    // «Смоки Мо» и «Lil Kate» остаются двумя исполнителями, а не одним с запятой.
    entries.extend(meta.artists.iter().map(|name| format!("ARTIST={name}")));
    if let Some(album) = &meta.album {
        entries.push(format!("ALBUM={album}"));
    }
    entries.extend(
        meta.album_artists
            .iter()
            .map(|name| format!("ALBUMARTIST={name}")),
    );
    if let Some(year) = meta.year {
        entries.push(format!("DATE={year}"));
    }
    if let Some(genre) = &meta.genre {
        entries.push(format!("GENRE={genre}"));
    }
    if let Some(number) = meta.number {
        entries.push(format!("TRACKNUMBER={}", number.index));
        entries.extend(number.total.map(|total| format!("TRACKTOTAL={total}")));
    }
    entries.extend(meta.volume.map(|volume| format!("DISCNUMBER={volume}")));

    entries
}

fn length(value: usize) -> [u8; 4] {
    u32::try_from(value).unwrap_or(u32::MAX).to_le_bytes()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::super::{Meta, Outcome, Reason, tests::sample};
    use super::{COMMENT, MAGIC, blocks, patch, verify};

    /// Минимальный `FLAC`: `STREAMINFO` (он же последний) и кадры за ним.
    fn file(extra: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        let mut all = vec![(0_u8, vec![0x11; 34])];
        all.extend_from_slice(extra);

        let last = all.len() - 1;
        for (index, (kind, body)) in all.iter().enumerate() {
            let flag = if index == last { kind | 0x80 } else { *kind };
            out.push(flag);
            out.extend(
                u32::try_from(body.len())
                    .unwrap()
                    .to_be_bytes()
                    .into_iter()
                    .skip(1),
            );
            out.extend_from_slice(body);
        }
        out.extend_from_slice(&[0xFF, 0xF8, 0x00, 0x00]);
        out
    }

    fn ready(bytes: &[u8], meta: &Meta) -> Option<(Vec<u8>, usize)> {
        match patch(bytes, meta) {
            Ok(Outcome::Ready { head, used }) => Some((head, used)),
            _other => None,
        }
    }

    #[test]
    fn inserts_a_comment_right_after_streaminfo() {
        let (head, used) = ready(&file(&[]), &sample()).unwrap();

        assert!(verify(&head).is_ok());
        let parsed = blocks(&head).unwrap().unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed.get(1).map(|block| block.kind), Some(COMMENT));
        // Кадры начинаются сразу за метаданными и переписыванию не подлежат.
        assert_eq!(used, file(&[]).len() - 4);
    }

    #[test]
    fn writes_every_field_it_was_given() {
        let (head, _used) = ready(&file(&[]), &sample()).unwrap();
        let text = String::from_utf8_lossy(&head).into_owned();

        for expected in [
            "TITLE=Сорок седьмой этаж",
            "ARTIST=Ветер Овна",
            "ALBUM=Пыль на антресолях",
            "DATE=2018",
            "GENRE=rap",
            "TRACKNUMBER=3",
            "TRACKTOTAL=12",
            "DISCNUMBER=1",
        ] {
            assert!(text.contains(expected), "нет поля {expected}");
        }
    }

    /// Прежний комментарий заменяется, прочие блоки остаются на месте:
    /// `SEEKTABLE` терять нельзя, он ускоряет перемотку.
    #[test]
    fn replaces_an_existing_comment_and_keeps_the_rest() {
        let original = file(&[
            (COMMENT, b"\x00\x00\x00\x00\x00\x00\x00\x00".to_vec()),
            (3, vec![0x22; 18]),
        ]);
        let (head, _used) = ready(&original, &sample()).unwrap();

        let parsed = blocks(&head).unwrap().unwrap();
        assert_eq!(
            parsed.iter().map(|block| block.kind).collect::<Vec<u8>>(),
            vec![0, COMMENT, 3]
        );
        assert!(verify(&head).is_ok());
    }

    #[test]
    fn asks_for_more_until_the_last_block_arrives() {
        let original = file(&[]);
        assert!(matches!(
            patch(original.get(..10).unwrap(), &sample()),
            Ok(Outcome::More)
        ));
    }

    #[test]
    fn refuses_what_is_not_flac() {
        assert_eq!(patch(b"OggS0000", &sample()).err(), Some(Reason::Malformed));
    }
}
