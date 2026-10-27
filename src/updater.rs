//! Что уже лежит в директории назначения.
//!
//! Отвечает на единственный вопрос: качать этот трек или он уже на месте.
//! Ответ берётся из снимка директории, снятого **до** начала выгрузки, и это
//! важно дважды. Во-первых, уже скачанный трек не стоит запроса к API:
//! расширение файла известно только из ответа `get-file-info`, поэтому проверка
//! «есть ли файл» без снимка обошлась бы в запрос на каждый трек. Во-вторых,
//! файл, записанный в этом же прогоне, в снимок не попал — одноимённый трек его
//! перезапишет, а не будет молча пропущен.

use std::{
    collections::HashSet,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use anyhow::Context as _;

use crate::track::Layout;

/// Что делать с треком, который уже лежит в директории.
///
/// Enum, а не `bool`: у флага в сигнатуре нет имени, а у варианта есть.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Качать весь список, перезаписывая существующие файлы.
    Full,
    /// Дозакачивать: пропускать то, что уже есть.
    Update,
}

/// Куда качать и что делать с уже лежащим там.
///
/// Эти двое всегда ездят вместе, поэтому ездят одним параметром.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Destination<'a> {
    pub(crate) directory: &'a Path,
    pub(crate) mode: Mode,
    pub(crate) layout: Layout,
}

impl Destination<'_> {
    /// Готовит директорию к работе и, если включено дозакачивание, снимает её
    /// содержимое.
    ///
    /// Вызывается до похода в сеть намеренно: опечатка в `--path` не должна
    /// стоить полной выкачки списка лайков.
    ///
    /// # Errors
    ///
    /// Ошибка, если директорию не создать или не прочитать.
    pub(crate) async fn prepare(&self) -> anyhow::Result<Option<Existing>> {
        tokio::fs::create_dir_all(self.directory)
            .await
            .with_context(|| {
                format!("не удалось создать директорию {}", self.directory.display())
            })?;

        match self.mode {
            Mode::Full => Ok(None),
            Mode::Update => Existing::read(self.directory, self.layout).await.map(Some),
        }
    }
}

/// Снимок содержимого директории.
#[derive(Debug)]
pub(crate) struct Existing {
    /// Имена файлов как есть — то, что видно в директории.
    names: HashSet<PathBuf>,
    /// Они же без расширения. Расширение до ответа API неизвестно, поэтому
    /// сверяться приходится по основе имени.
    stems: HashSet<PathBuf>,
}

impl Existing {
    /// Читает снимок один раз; для structured спускается только до альбомов.
    ///
    /// # Errors
    ///
    /// Ошибка, если директорию не открыть или не перечислить.
    pub(crate) async fn read(directory: &Path, layout: Layout) -> anyhow::Result<Self> {
        let max_depth = match layout {
            Layout::Flatten => 0,
            Layout::Structured => 2,
        };
        let mut pending = vec![PathBuf::new()];
        let mut names = Vec::new();
        while let Some(prefix) = pending.pop() {
            let current = directory.join(&prefix);
            let mut entries = tokio::fs::read_dir(&current).await.with_context(|| {
                format!("не удалось прочитать директорию {}", current.display())
            })?;
            while let Some(entry) = entries.next_entry().await.with_context(|| {
                format!("не удалось перечислить директорию {}", current.display())
            })? {
                // Не идём по симлинкам: ни циклы, ни чужое дерево в снимок не нужны.
                let kind = entry.file_type().await.with_context(|| {
                    format!("не удалось определить тип {}", entry.path().display())
                })?;
                let relative = prefix.join(entry.file_name());
                if kind.is_file() {
                    names.push(relative);
                } else if kind.is_dir() && prefix.components().count() < max_depth {
                    pending.push(relative);
                }
            }
        }

        Ok(Self::from_names(names))
    }

    /// Имена файлов, найденных в директории.
    pub(crate) fn names(&self) -> &HashSet<PathBuf> {
        &self.names
    }

    /// Есть ли файл с такой основой имени — при любом расширении.
    pub(crate) fn contains_stem(&self, stem: impl AsRef<Path>) -> bool {
        self.stems.contains(stem.as_ref())
    }

    fn from_names(names: impl IntoIterator<Item = impl AsRef<Path>>) -> Self {
        let names: HashSet<PathBuf> = names
            .into_iter()
            .map(|name| name.as_ref().to_owned())
            .collect();
        let stems = names
            .iter()
            .filter(|name| name.extension() != Some(OsStr::new("part")))
            .filter_map(|name| name.file_stem().map(|stem| name.with_file_name(stem)))
            .collect();

        Self { names, stems }
    }
}

#[cfg(test)]
mod tests {
    use super::Existing;

    #[test]
    fn test_indexes_relative_stem_without_losing_directories_or_dots() {
        let existing = Existing::from_names(["Burial/Untrue/Archangel.live.flac"]);

        assert!(existing.contains_stem("Burial/Untrue/Archangel.live"));
    }

    #[test]
    fn test_partial_file_does_not_match_a_title_ending_in_a_codec() {
        let existing = Existing::from_names(["Burial/Untrue/Archangel.mp3.part"]);

        assert!(!existing.contains_stem("Burial/Untrue/Archangel.mp3"));
    }

    #[test]
    fn indexes_existing_files_by_stem() {
        let existing = Existing::from_names(["Burial - Archangel.flac", "заметка.txt"]);

        assert!(existing.contains_stem("Burial - Archangel"));
        assert!(existing.contains_stem("заметка"));
        assert!(!existing.contains_stem("Burial - Untrue"));
    }

    #[test]
    fn test_keeps_file_names_as_they_are() {
        let existing = Existing::from_names(["Burial - Archangel.flac"]);

        assert!(
            existing
                .names()
                .contains(std::path::Path::new("Burial - Archangel.flac"))
        );
        assert_eq!(existing.names().len(), 1);
    }

    /// Огрызок оборванной загрузки не должен сойти за скачанный трек — иначе
    /// повтор команды закрепил бы порчу вместо того, чтобы её исправить.
    /// Держится на том, что `file_stem` у `X.flac.part` даёт `X.flac`, а не `X`.
    #[test]
    fn does_not_mistake_partial_download_for_a_finished_file() {
        let existing = Existing::from_names(["Burial - Archangel.flac.part"]);

        assert!(!existing.contains_stem("Burial - Archangel"));
    }
}
