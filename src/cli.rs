//! Разбор аргументов командной строки.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum, builder::PossibleValue};

use crate::{
    download::Quality,
    updater::{Destination, Mode},
};

#[derive(Debug, Parser)]
#[command(
    name = "ymelorix",
    about = "Выгрузка избранного из Яндекс Музыки",
    version
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Загрузить все избранные треки
    // `override_usage` вместо клапового `[OPTIONS]`: осмысленные опции видно
    // прямо в строке Usage, а не только в списке ниже. Строка написана руками,
    // поэтому новый аргумент нужно дописать сюда же — сама она не обновится.
    #[command(
        override_usage = "ymelorix download-favorites [--path <DIR>] [--quality <QUALITY>] [--update]"
    )]
    DownloadFavorites {
        #[command(flatten)]
        target: Target,
    },
    /// Вывести все избранные треки
    ListFavorites,
    /// Загрузить трек, альбом или плейлист по ссылке
    #[command(
        override_usage = "ymelorix download-link <URL> [--path <DIR>] [--quality <QUALITY>] [--update]"
    )]
    DownloadLink {
        /// Ссылка на трек, альбом или плейлист в Яндекс Музыке
        #[arg(help = "Ссылка вида https://music.yandex.ru/track/<id>, \
                    https://music.yandex.ru/album/<id> или \
                    https://music.yandex.ru/users/<владелец>/playlists/<номер>")]
        url: String,

        #[command(flatten)]
        target: Target,
    },
}

/// Куда складывать файлы и что делать с уже лежащими. Общая часть обеих команд
/// загрузки.
#[derive(Debug, Args)]
pub(crate) struct Target {
    /// Директория для файлов, создаётся при необходимости
    #[arg(short, long, value_name = "DIR", default_value = ".")]
    pub(crate) path: PathBuf,

    /// Качество: lossless, high, normal или low
    #[arg(short, long, value_name = "QUALITY", default_value = "lossless")]
    pub(crate) quality: Quality,

    /// Дозакачать: пропустить то, что уже есть в директории
    #[arg(short, long)]
    pub(crate) update: bool,
}

impl Target {
    /// Переводит аргументы в назначение на границе разбора: дальше по коду
    /// ходит режим с именем, а не голый `bool`.
    pub(crate) fn destination(&self) -> Destination<'_> {
        let Self {
            path,
            quality: _quality,
            update,
        } = self;

        Destination {
            directory: path,
            mode: if *update { Mode::Update } else { Mode::Full },
        }
    }
}

/// Реализация написана руками, а не выведена: `Quality` живёт в `download`, и
/// знание о clap не должно спускаться туда — слой разбора аргументов здесь.
impl ValueEnum for Quality {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Lossless, Self::High, Self::Normal, Self::Low]
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        Some(match self {
            Self::Lossless => {
                PossibleValue::new("lossless").help("без потерь, FLAC — самые большие файлы")
            }
            Self::High => PossibleValue::new("high").help("высокое, MP3 320 kbps"),
            Self::Normal => PossibleValue::new("normal").help("обычное, экономит трафик и место"),
            Self::Low => PossibleValue::new("low").help("низкое, для медленной связи"),
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "тесты: несработавший unwrap и есть провал теста"
)]
mod tests {
    use clap::Parser as _;
    use rstest::rstest;

    use super::{Cli, Command, Mode, Quality, Target};

    /// Возвращает `None`, если разбор не удался или команда оказалась не той:
    /// `expect` и `panic!` запрещены и в тестах.
    fn target_of(args: &[&str]) -> Option<Target> {
        match Cli::try_parse_from(args).ok()?.command {
            Command::DownloadFavorites { target } | Command::DownloadLink { target, .. } => {
                Some(target)
            }
            Command::ListFavorites => None,
        }
    }

    /// Умолчания — часть договорённости с пользователем: без флагов качается
    /// весь список и без потерь.
    #[test]
    fn defaults_to_lossless_and_full_download() {
        let target = target_of(&["ymelorix", "download-favorites"]).unwrap();

        assert_eq!(
            (target.quality, target.destination().mode),
            (Quality::Lossless, Mode::Full)
        );
    }

    /// Имена уровней — часть договорённости с пользователем, и `high` от
    /// `normal` отличается только на проводе: перепутать их местами разбор не
    /// должен.
    #[rstest]
    #[case::lossless("lossless", Quality::Lossless)]
    #[case::high("high", Quality::High)]
    #[case::normal("normal", Quality::Normal)]
    #[case::low("low", Quality::Low)]
    fn reads_every_quality(#[case] argument: &str, #[case] expected: Quality) {
        let target = target_of(&["ymelorix", "download-favorites", "-q", argument]).unwrap();

        assert_eq!(target.quality, expected);
    }

    #[test]
    fn reads_quality_and_update_flag() {
        let target = target_of(&[
            "ymelorix",
            "download-link",
            "33898323",
            "--quality",
            "low",
            "--update",
        ])
        .unwrap();

        assert_eq!(
            (target.quality, target.destination().mode),
            (Quality::Low, Mode::Update)
        );
    }

    #[test]
    fn rejects_unknown_quality() {
        let parsed = Cli::try_parse_from(["ymelorix", "download-favorites", "-q", "ultra"]);

        assert!(parsed.is_err());
    }
}
