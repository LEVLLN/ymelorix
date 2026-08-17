//! Плоское представление трека для вывода и скачивания.

use core::fmt;

use anyhow::Context as _;
use yandex_music::model::{
    album::{Album, TrackPosition},
    track::Track,
};

use crate::tags;

const UNKNOWN_ARTIST: &str = "<неизвестный исполнитель>";
const UNTITLED: &str = "<без названия>";

/// Символы, недопустимые в именах файлов хотя бы на одной из целевых ФС.
const FORBIDDEN_IN_NAME: [char; 9] = ['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

/// Предел на «исполнители - название» в байтах.
///
/// У большинства файловых систем имя ограничено 255 **байтами**, а не
/// символами: кириллица занимает по два байта, эмодзи — по четыре, так что сто
/// символов названия могли бы дать четыреста байт и `ENAMETOOLONG`. Остаток
/// бюджета уходит на расширение и суффикс незавершённой загрузки: `.flac.part`.
const MAX_NAME_BYTES: usize = 150;

/// Идентификатор трека в Музыке.
///
/// Разбирается один раз на границе — дальше по коду ходит уже проверенным.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrackId(String);

impl TrackId {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// Достаёт идентификатор из ссылки на трек.
    ///
    /// Понимает `music.yandex.ru/album/<id>/track/<id>`, `music.yandex.ru/track/<id>`
    /// и голый идентификатор.
    ///
    /// # Errors
    ///
    /// Ошибка, если в строке нет сегмента `track/<цифры>` и она сама не является
    /// идентификатором.
    pub(crate) fn parse(input: &str) -> anyhow::Result<Self> {
        let input = input.trim();
        if is_id(input) {
            return Ok(Self(input.to_owned()));
        }

        let path = input.split(['?', '#']).next().unwrap_or(input);
        path.split('/')
            .skip_while(|segment| *segment != "track")
            .nth(1)
            .filter(|id| is_id(id))
            .map(|id| Self(id.to_owned()))
            .with_context(|| format!("в ссылке нет идентификатора трека: {input}"))
    }
}

impl fmt::Display for TrackId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn is_id(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|symbol| symbol.is_ascii_digit())
}

/// Место трека в альбоме: номер диска и номер на диске.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Position {
    volume: u16,
    index: u16,
}

/// Альбом, из которого взят трек, — всё, что нужно тегам.
///
/// Приезжает тем же ответом `get-tracks`, что и сам трек: за теги не платится
/// ни одного лишнего запроса.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AlbumInfo {
    title: Option<String>,
    artists: Vec<String>,
    year: Option<u16>,
    genre: Option<String>,
    position: Option<Position>,
    tracks: Option<u16>,
}

/// Модель крейта дальше этого места не идёт: форматирование остаётся чистым
/// и проверяется тестами без обращения к API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrackInfo {
    id: TrackId,
    artists: Vec<String>,
    title: Option<String>,
    album: Option<AlbumInfo>,
}

impl TrackInfo {
    pub(crate) fn id(&self) -> &TrackId {
        &self.id
    }

    /// Что писать в теги файла.
    ///
    /// Заглушки `<неизвестный исполнитель>` и `<без названия>` сюда не едут:
    /// в имени файла они полезны, а в теге плеер покажет их как настоящее имя.
    pub(crate) fn tags(&self) -> tags::Meta {
        let album = self.album.as_ref();

        tags::Meta {
            title: cleaned(self.title.as_deref()),
            artists: owned(&self.artists),
            album: album.and_then(|album| cleaned(album.title.as_deref())),
            album_artists: album.map(|album| owned(&album.artists)).unwrap_or_default(),
            year: album.and_then(|album| album.year),
            genre: album.and_then(|album| cleaned(album.genre.as_deref())),
            number: album.and_then(|album| {
                album.position.map(|position| tags::Number {
                    index: position.index,
                    total: album.tracks,
                })
            }),
            volume: album.and_then(|album| album.position.map(|position| position.volume)),
        }
    }

    /// Имя файла без расширения: `исполнители - название`.
    ///
    /// Идентификатора в имени намеренно нет — имя должно оставаться читаемым.
    /// Плата за это: два разных трека с одинаковым названием делят путь, и
    /// внутри одной выгрузки второй молча перезаписывает первого (снимок
    /// директории в [`crate::updater::Existing`] снимается до начала работы,
    /// поэтому свежезаписанный файл пропуска не вызывает).
    ///
    /// Запрещённые символы заменяются на `_`, длина ограничена по байтам —
    /// иначе трек с длинным названием не запишется на диск.
    pub(crate) fn file_stem(&self) -> String {
        let name: String = format!("{} - {}", self.artists(), self.title())
            .chars()
            .map(|symbol| {
                if FORBIDDEN_IN_NAME.contains(&symbol) || symbol.is_control() {
                    '_'
                } else {
                    symbol
                }
            })
            .collect();

        // `trim_end` после обрезки: срез по байтовому пределу легко приходится
        // на пробел, а имя файла с хвостовым пробелом — источник сюрпризов.
        truncate_bytes(&name, MAX_NAME_BYTES).trim_end().to_owned()
    }

    /// Исполнители одной строкой.
    ///
    /// Пустая строка от API — это отсутствие, а не имя: иначе трек без
    /// исполнителя получил бы файл, начинающийся с пробела и дефиса.
    fn artists(&self) -> String {
        match owned(&self.artists).as_slice() {
            [] => UNKNOWN_ARTIST.to_owned(),
            named => named.join(", "),
        }
    }

    /// Название трека, с тем же отношением к пустой строке.
    fn title(&self) -> &str {
        self.title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .unwrap_or(UNTITLED)
    }
}

/// Строка от API без краёв — или ничего, если после обрезки не осталось ничего.
///
/// Пустая строка от API — это отсутствие данных, а не значение.
fn cleaned(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Имена без пустых и без краёв.
fn owned(names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter_map(|name| cleaned(Some(name)))
        .collect()
}

/// Режет строку по пределу в байтах, не разваливая символ пополам.
fn truncate_bytes(text: &str, limit: usize) -> String {
    text.char_indices()
        .take_while(|(offset, symbol)| offset + symbol.len_utf8() <= limit)
        .map(|(_offset, symbol)| symbol)
        .collect()
}

/// Ответ Музыки не лёг в модель.
///
/// Числа, не помещающиеся в номер трека, — это не «редкий альбом», а мусор в
/// ответе. Отбросить их молча — значит записать в тег правдоподобную неправду,
/// поэтому конверсия падающая и живёт в [`TryFrom`], а не в [`From`].
#[derive(Debug, thiserror::Error)]
pub(crate) enum Malformed {
    #[error("номер трека в альбоме не похож на номер: {0}")]
    Position(u32),
    #[error("число треков в альбоме не похоже на число: {0}")]
    Count(u32),
}

impl TryFrom<&Track> for TrackInfo {
    type Error = Malformed;

    fn try_from(track: &Track) -> Result<Self, Self::Error> {
        Ok(Self {
            id: TrackId(track.id.clone()),
            artists: names(&track.artists),
            title: track.title.clone(),
            // Альбомов у трека может быть несколько (сингл, сборник,
            // переиздание); первый — тот, в контексте которого трек отдан.
            album: track.albums.first().map(AlbumInfo::try_from).transpose()?,
        })
    }
}

impl TryFrom<&Album> for AlbumInfo {
    type Error = Malformed;

    fn try_from(album: &Album) -> Result<Self, Self::Error> {
        Ok(Self {
            title: album.title.clone(),
            artists: names(&album.artists),
            year: album.year,
            genre: album.genre.clone(),
            position: album
                .track_position
                .as_ref()
                .map(Position::try_from)
                .transpose()?,
            tracks: album
                .track_count
                .map(|count| u16::try_from(count).map_err(|_overflow| Malformed::Count(count)))
                .transpose()?,
        })
    }
}

impl TryFrom<&TrackPosition> for Position {
    type Error = Malformed;

    fn try_from(position: &TrackPosition) -> Result<Self, Self::Error> {
        // Разбор ради проверки на полноту: новое поле сломает компиляцию здесь.
        let TrackPosition { volume, index } = position;

        Ok(Self {
            volume: u16::from(*volume),
            index: u16::try_from(*index).map_err(|_overflow| Malformed::Position(*index))?,
        })
    }
}

fn names(artists: &[yandex_music::model::artist::Artist]) -> Vec<String> {
    artists
        .iter()
        .filter_map(|artist| artist.name.clone())
        .collect()
}

impl fmt::Display for TrackInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Разбор ради проверки на полноту: новое поле сломает компиляцию здесь,
        // а не тихо пропадёт из вывода.
        let Self {
            id: _id,
            artists: _artists,
            title: _title,
            album: _album,
        } = self;

        write!(f, "{} — {}", self.artists(), self.title())
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::{AlbumInfo, Position, TrackId, TrackInfo};

    fn track(artists: &[&str], title: Option<&str>) -> TrackInfo {
        TrackInfo {
            id: TrackId("1".to_owned()),
            artists: artists.iter().map(|name| (*name).to_owned()).collect(),
            title: title.map(str::to_owned),
            album: None,
        }
    }

    fn with_id(id: &str, artists: &[&str], title: Option<&str>) -> TrackInfo {
        let TrackInfo {
            id: _generated,
            artists,
            title,
            album,
        } = track(artists, title);

        TrackInfo {
            id: TrackId(id.to_owned()),
            artists,
            title,
            album,
        }
    }

    #[rstest]
    #[case::full(track(&["Radiohead"], Some("Weird Fishes")), "Radiohead — Weird Fishes")]
    #[case::several_artists(
        track(&["Emika", "Michal Wolski"], Some("Cooler")),
        "Emika, Michal Wolski — Cooler"
    )]
    #[case::no_artists(track(&[], Some("Untitled")), "<неизвестный исполнитель> — Untitled")]
    #[case::no_title(track(&["Burial"], None), "Burial — <без названия>")]
    #[case::nothing_known(track(&[], None), "<неизвестный исполнитель> — <без названия>")]
    fn displays_track(#[case] track: TrackInfo, #[case] expected: &str) {
        assert_eq!(track.to_string(), expected);
    }

    #[rstest]
    #[case::plain(track(&["Burial"], Some("Archangel")), "Burial - Archangel")]
    #[case::several_artists(
        track(&["Emika", "Michal Wolski"], Some("Cooler")),
        "Emika, Michal Wolski - Cooler"
    )]
    #[case::strips_slashes(track(&["AC/DC"], Some("Who Made Who?")), "AC_DC - Who Made Who_")]
    #[case::strips_control(track(&["A\nB"], Some("C\tD")), "A_B - C_D")]
    fn builds_file_stem(#[case] track: TrackInfo, #[case] expected: &str) {
        assert_eq!(track.file_stem(), expected);
    }

    /// Решение принято сознательно: читаемое имя важнее развода одноимённых
    /// треков, а совпадение разбирается перезаписью и записью в лог.
    #[test]
    fn gives_equally_titled_tracks_the_same_name() {
        let original = with_id("1", &["Nirvana"], Some("Lithium"));
        let remaster = with_id("2", &["Nirvana"], Some("Lithium"));

        assert_eq!(original.file_stem(), remaster.file_stem());
    }

    #[rstest]
    #[case::cyrillic("о")]
    #[case::emoji("🎧")]
    #[case::ascii("o")]
    fn keeps_file_stem_within_byte_budget(#[case] symbol: &str) {
        let track = track(&["Разработчик"], Some(&symbol.repeat(500)));

        let stem = track.file_stem();
        assert!(
            stem.len() <= super::MAX_NAME_BYTES,
            "имя заняло {} байт: {stem}",
            stem.len()
        );
    }

    /// Пустая строка от API — это отсутствие данных, а не имя: без этого
    /// файл назывался бы « - » и склеивал все такие треки в один.
    #[rstest]
    #[case::blank_artist(
        track(&["   "], Some("Untitled")),
        "_неизвестный исполнитель_ - Untitled"
    )]
    #[case::blank_title(track(&["Burial"], Some("  ")), "Burial - _без названия_")]
    fn treats_blank_values_from_api_as_missing(#[case] track: TrackInfo, #[case] expected: &str) {
        assert_eq!(track.file_stem(), expected);
    }

    /// Никакое название не должно выводить запись за пределы указанной
    /// директории: сегодня это держится на замене `/` и `\`, а тест закрепляет
    /// свойство целиком.
    #[rstest]
    #[case::traversal("../../etc/passwd")]
    #[case::absolute("/etc/passwd")]
    #[case::windows_separator("..\\..\\windows")]
    #[case::dots("..")]
    fn file_stem_stays_a_single_path_component(#[case] title: &str) {
        let stem = track(&["Кто-то"], Some(title)).file_stem();

        let path = std::path::Path::new("/базовая").join(&stem);
        assert_eq!(
            path.file_name().and_then(std::ffi::OsStr::to_str),
            Some(&*stem)
        );
        assert_eq!(path.parent(), Some(std::path::Path::new("/базовая")));
    }

    #[test]
    fn carries_album_data_into_tags() {
        let track = TrackInfo {
            id: TrackId("1".to_owned()),
            artists: vec!["Кремний".to_owned(), "  Тальк  ".to_owned()],
            title: Some("  Обратный отсчёт  ".to_owned()),
            album: Some(AlbumInfo {
                title: Some("Ниже уровня моря".to_owned()),
                artists: vec!["Кремний".to_owned()],
                year: Some(2019),
                genre: Some("rap".to_owned()),
                position: Some(Position {
                    volume: 1,
                    index: 4,
                }),
                tracks: Some(11),
            }),
        };

        let meta = track.tags();
        assert_eq!(meta.title, Some("Обратный отсчёт".to_owned()));
        assert_eq!(meta.artists, vec!["Кремний".to_owned(), "Тальк".to_owned()]);
        assert_eq!(meta.album, Some("Ниже уровня моря".to_owned()));
        assert_eq!(
            meta.number,
            Some(crate::tags::Number {
                index: 4,
                total: Some(11)
            })
        );
        assert_eq!(meta.volume, Some(1));
    }

    /// Заглушки нужны имени файла, но не тегу: `<без названия>` в плеере
    /// выглядит как настоящее название, а отсутствие поля — как отсутствие.
    #[test]
    fn keeps_placeholders_out_of_tags() {
        let meta = track(&["   "], None).tags();

        assert_eq!(meta.title, None);
        assert!(meta.artists.is_empty());
        assert_eq!(meta.number, None);
    }

    #[rstest]
    #[case::album_and_track("https://music.yandex.ru/album/4147089/track/33898323", "33898323")]
    #[case::track_only("https://music.yandex.ru/track/33898323", "33898323")]
    #[case::with_query("https://music.yandex.ru/album/1/track/42?utm_source=web", "42")]
    #[case::with_fragment("https://music.yandex.ru/album/1/track/42#play", "42")]
    #[case::no_scheme("music.yandex.ru/album/1/track/42", "42")]
    #[case::bare_id("33898323", "33898323")]
    #[case::spaces_around("  33898323  ", "33898323")]
    fn parses_track_id(#[case] input: &str, #[case] expected: &str) {
        let parsed = TrackId::parse(input).map(|id| id.as_str().to_owned());
        assert_eq!(parsed.ok(), Some(expected.to_owned()));
    }

    #[rstest]
    #[case::album_link("https://music.yandex.ru/album/4147089")]
    #[case::artist_link("https://music.yandex.ru/artist/9262")]
    #[case::track_without_id("https://music.yandex.ru/album/1/track/")]
    #[case::non_numeric_id("https://music.yandex.ru/album/1/track/abc")]
    #[case::empty("")]
    fn rejects_bad_track_link(#[case] input: &str) {
        assert!(TrackId::parse(input).is_err());
    }
}
