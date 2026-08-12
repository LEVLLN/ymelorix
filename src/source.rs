//! Ссылка из Музыки: трек, альбом или плейлист.
//!
//! Разбор чистый и живёт отдельно от запросов: по ссылке уже на границе видно,
//! какую ручку звать, а ошибка про непонятную ссылку не стоит ни одного запроса.
//! Команда загрузки одна, поэтому и разбор один: пользователь вставляет ссылку,
//! а не выбирает подкоманду под её вид.

use core::fmt;

use anyhow::bail;

use crate::track::TrackId;

/// Что скачивать по ссылке.
///
/// Enum, а не набор `Option`: ссылка ведёт ровно на одно из трёх, и «альбом и
/// трек сразу» или «ни одного» выразить нельзя.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Source {
    Track(TrackId),
    Album(AlbumId),
    Playlist(PlaylistId),
}

/// Идентификатор альбома. Ручка крейта принимает `u32` — им и разбираем.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AlbumId(u32);

impl AlbumId {
    pub(crate) const fn value(&self) -> u32 {
        self.0
    }
}

/// Плейлист адресуется парой «владелец и номер».
///
/// Владелец — строка, а не число: в ссылках стоит логин (`yamusic-daily`), и
/// числовой `uid` там встречается наравне с ним. Ручка принимает оба.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlaylistId {
    owner: String,
    kind: u32,
}

impl PlaylistId {
    pub(crate) fn owner(&self) -> &str {
        &self.owner
    }

    pub(crate) const fn kind(&self) -> u32 {
        self.kind
    }
}

impl Source {
    /// Разбирает ссылку на трек, альбом или плейлист.
    ///
    /// Понимает `music.yandex.ru/track/<id>`, `music.yandex.ru/album/<id>`,
    /// `music.yandex.ru/album/<id>/track/<id>` и
    /// `music.yandex.ru/users/<владелец>/playlists/<номер>` — со схемой и без,
    /// с параметрами запроса и якорем. Голое число — это трек: у альбома и
    /// плейлиста голой формы нет, поэтому двусмысленности не возникает.
    ///
    /// # Errors
    ///
    /// Ошибка, если ссылка ведёт на новый плейлист без владельца или не
    /// разбирается вовсе. Текст отказа называет форму ссылки, которая подойдёт:
    /// пользователь на этом месте уже держит ссылку в руках.
    pub(crate) fn parse(input: &str) -> anyhow::Result<Self> {
        let input = input.trim();
        let path = input.split(['?', '#']).next().unwrap_or(input);
        let segments: Vec<&str> = path.split('/').collect();

        // Сказано «трек» — разбираем как трек и не отступаем: у ссылки
        // `album/<id>/track/<...>` иначе нашёлся бы номер альбома, и испорченный
        // номер трека молча превратился бы в загрузку целого диска.
        let bare_id = matches!(segments.as_slice(), [single] if !single.is_empty()
            && single.chars().all(|symbol| symbol.is_ascii_digit()));
        if segments.contains(&"track") || bare_id {
            return TrackId::parse(input).map(Self::Track);
        }

        if let Some(id) = after(&segments, "album") {
            return id
                .parse()
                .map(|id| Self::Album(AlbumId(id)))
                .map_err(|_not_a_number| anyhow::anyhow!("в ссылке нет номера альбома: {input}"));
        }

        match (after(&segments, "users"), after(&segments, "playlists")) {
            (Some(owner), Some(kind)) if is_owner(owner) => kind
                .parse()
                .map(|kind| {
                    Self::Playlist(PlaylistId {
                        owner: owner.to_owned(),
                        kind,
                    })
                })
                .map_err(|_not_a_number| anyhow::anyhow!("в ссылке нет номера плейлиста: {input}")),
            // Плейлист без владельца — новая форма ссылки, `.../playlists/<uuid>`.
            // Ручка API просит владельца и номер, взять их из uuid неоткуда.
            (None | Some(_), Some(_)) => bail!(
                "плейлист адресуется владельцем и номером: \
                 music.yandex.ru/users/<владелец>/playlists/<номер>. \
                 Такую ссылку даёт кнопка «Поделиться» в плейлисте: {input}"
            ),
            (Some(_) | None, None) => {
                bail!("ссылка не похожа ни на трек, ни на альбом, ни на плейлист: {input}")
            }
        }
    }
}

/// Годится ли строка на роль владельца плейлиста.
///
/// Проверка не косметическая: владелец приходит из ссылки и **уходит в путь
/// запроса**. Без неё `%2f`, точки и прочая пунктуация в логине увели бы запрос
/// на соседнюю ручку API. Логины Яндекса — латиница, цифры, дефис, точка и
/// подчёркивание; числовой `uid` под то же описание подходит.
fn is_owner(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|symbol| symbol.is_ascii_alphanumeric() || matches!(symbol, '-' | '.' | '_'))
}

/// Сегмент пути, идущий сразу за названным.
fn after<'a>(segments: &[&'a str], name: &str) -> Option<&'a str> {
    segments
        .iter()
        .skip_while(|segment| **segment != name)
        .nth(1)
        .copied()
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Track(id) => write!(f, "трек {id}"),
            Self::Album(AlbumId(id)) => write!(f, "альбом {id}"),
            Self::Playlist(PlaylistId { owner, kind }) => write!(f, "плейлист {owner}/{kind}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::Source;

    #[rstest]
    #[case::album("https://music.yandex.ru/album/4147089", "альбом 4147089")]
    #[case::album_with_query(
        "https://music.yandex.ru/album/4147089?utm_source=web",
        "альбом 4147089"
    )]
    #[case::album_no_scheme("music.yandex.ru/album/4147089", "альбом 4147089")]
    #[case::album_trailing_slash("https://music.yandex.ru/album/4147089/", "альбом 4147089")]
    #[case::playlist(
        "https://music.yandex.ru/users/yamusic-daily/playlists/1234",
        "плейлист yamusic-daily/1234"
    )]
    #[case::playlist_with_fragment(
        "https://music.yandex.ru/users/ivan/playlists/3#play",
        "плейлист ivan/3"
    )]
    #[case::playlist_numeric_owner(
        "https://music.yandex.ru/users/1130000012345678/playlists/3",
        "плейлист 1130000012345678/3"
    )]
    #[case::spaces_around("  https://music.yandex.ru/album/1  ", "альбом 1")]
    fn parses_link(#[case] input: &str, #[case] expected: &str) {
        let parsed = Source::parse(input).map(|source| source.to_string());
        assert_eq!(parsed.ok(), Some(expected.to_owned()));
    }

    /// Ссылка на трек внутри альбома — это трек, а не альбом. Иначе одна
    /// лишняя часть пути молча превращала бы загрузку трека в загрузку диска.
    #[rstest]
    #[case::track_in_album(
        "https://music.yandex.ru/album/4147089/track/33898323",
        "трек 33898323"
    )]
    #[case::track_alone("https://music.yandex.ru/track/33898323", "трек 33898323")]
    #[case::track_with_query("https://music.yandex.ru/track/42?utm_source=web", "трек 42")]
    #[case::bare_id("33898323", "трек 33898323")]
    #[case::bare_id_with_spaces("  33898323  ", "трек 33898323")]
    fn parses_track_link(#[case] input: &str, #[case] expected: &str) {
        let parsed = Source::parse(input).map(|source| source.to_string());
        assert_eq!(parsed.ok(), Some(expected.to_owned()));
    }

    #[rstest]
    #[case::artist("https://music.yandex.ru/artist/9262")]
    #[case::album_without_id("https://music.yandex.ru/album/")]
    #[case::album_not_a_number("https://music.yandex.ru/album/abc")]
    #[case::playlist_without_kind("https://music.yandex.ru/users/ivan/playlists/")]
    #[case::playlist_without_owner("https://music.yandex.ru/users//playlists/1")]
    #[case::playlist_kind_not_a_number("https://music.yandex.ru/users/ivan/playlists/abc")]
    // Номер альбома рядом с испорченным номером трека не должен вытянуть
    // разбор в сторону альбома: сказано «трек» — значит трек.
    #[case::track_not_a_number("https://music.yandex.ru/album/1/track/abc")]
    #[case::track_without_id("https://music.yandex.ru/album/1/track/")]
    #[case::empty("")]
    // Владелец уезжает в путь запроса, поэтому пунктуация в нём — это попытка
    // увести запрос на другую ручку, а не необычный логин.
    #[case::owner_escapes_the_path("https://music.yandex.ru/users/..%2f..%2faccount/playlists/1")]
    #[case::owner_with_query("https://music.yandex.ru/users/ivan%3Fx=1/playlists/1")]
    fn rejects_bad_link(#[case] input: &str) {
        assert!(Source::parse(input).is_err());
    }

    /// Новая форма ссылки на плейлист идёт без владельца, а ручка API просит
    /// именно владельца и `kind`: честный отказ лучше запроса, который всё
    /// равно не соберётся.
    #[test]
    fn explains_that_uuid_playlist_link_is_not_supported() {
        let message = Source::parse("https://music.yandex.ru/playlists/8a7b6c5d")
            .err()
            .map(|error| format!("{error:#}"));

        assert!(
            message.is_some_and(|message| message.contains("users/")),
            "отказ должен показывать поддерживаемую форму ссылки"
        );
    }
}
