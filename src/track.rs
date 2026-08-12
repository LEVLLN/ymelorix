//! Плоское представление трека для вывода и скачивания.

use core::fmt;

use anyhow::Context as _;
use yandex_music::model::track::Track;

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

/// Модель крейта дальше этого места не идёт: форматирование остаётся чистым
/// и проверяется тестами без обращения к API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrackInfo {
    id: TrackId,
    artists: Vec<String>,
    title: Option<String>,
}

impl TrackInfo {
    pub(crate) fn id(&self) -> &TrackId {
        &self.id
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
        let named: Vec<&str> = self
            .artists
            .iter()
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
            .collect();

        if named.is_empty() {
            UNKNOWN_ARTIST.to_owned()
        } else {
            named.join(", ")
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

/// Режет строку по пределу в байтах, не разваливая символ пополам.
fn truncate_bytes(text: &str, limit: usize) -> String {
    text.char_indices()
        .take_while(|(offset, symbol)| offset + symbol.len_utf8() <= limit)
        .map(|(_offset, symbol)| symbol)
        .collect()
}

impl From<&Track> for TrackInfo {
    fn from(track: &Track) -> Self {
        Self {
            id: TrackId(track.id.clone()),
            artists: track
                .artists
                .iter()
                .filter_map(|artist| artist.name.clone())
                .collect(),
            title: track.title.clone(),
        }
    }
}

impl fmt::Display for TrackInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Разбор ради проверки на полноту: новое поле сломает компиляцию здесь,
        // а не тихо пропадёт из вывода.
        let Self {
            id: _id,
            artists: _artists,
            title: _title,
        } = self;

        write!(f, "{} — {}", self.artists(), self.title())
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::{TrackId, TrackInfo};

    fn track(artists: &[&str], title: Option<&str>) -> TrackInfo {
        TrackInfo {
            id: TrackId("1".to_owned()),
            artists: artists.iter().map(|name| (*name).to_owned()).collect(),
            title: title.map(str::to_owned),
        }
    }

    fn with_id(id: &str, artists: &[&str], title: Option<&str>) -> TrackInfo {
        let TrackInfo {
            id: _generated,
            artists,
            title,
        } = track(artists, title);

        TrackInfo {
            id: TrackId(id.to_owned()),
            artists,
            title,
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
