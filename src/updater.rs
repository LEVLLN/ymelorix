//! Что уже лежит в директории назначения.
//!
//! Отвечает на единственный вопрос: качать этот трек или он уже на месте.
//! Ответ берётся из снимка директории, снятого **до** начала выгрузки, и это
//! важно дважды. Во-первых, уже скачанный трек не стоит запроса к API:
//! расширение файла известно только из ответа `get-file-info`, поэтому проверка
//! «есть ли файл» без снимка обошлась бы в запрос на каждый трек. Во-вторых,
//! файл, записанный в этом же прогоне, в снимок не попал — одноимённый трек его
//! перезапишет, а не будет молча пропущен.

use std::{collections::HashSet, ffi::OsStr, path::Path};

use anyhow::Context as _;

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
            Mode::Update => Existing::read(self.directory).await.map(Some),
        }
    }
}

/// Снимок содержимого директории.
#[derive(Debug)]
pub(crate) struct Existing {
    /// Имена файлов как есть — то, что видно в директории.
    names: HashSet<String>,
    /// Они же без расширения. Расширение до ответа API неизвестно, поэтому
    /// сверяться приходится по основе имени.
    stems: HashSet<String>,
}

impl Existing {
    /// Читает директорию целиком, один раз.
    ///
    /// # Errors
    ///
    /// Ошибка, если директорию не открыть или не перечислить.
    pub(crate) async fn read(directory: &Path) -> anyhow::Result<Self> {
        let mut entries = tokio::fs::read_dir(directory)
            .await
            .with_context(|| format!("не удалось прочитать директорию {}", directory.display()))?;

        let mut names = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .with_context(|| format!("не удалось перечислить директорию {}", directory.display()))?
        {
            names.push(entry.file_name());
        }

        Ok(Self::from_names(
            names.iter().filter_map(|name| name.to_str()),
        ))
    }

    /// Имена файлов, найденных в директории.
    pub(crate) fn names(&self) -> &HashSet<String> {
        &self.names
    }

    /// Есть ли файл с такой основой имени — при любом расширении.
    pub(crate) fn contains_stem(&self, stem: &str) -> bool {
        self.stems.contains(stem)
    }

    fn from_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        let names: HashSet<String> = names.into_iter().map(str::to_owned).collect();
        let stems = names
            .iter()
            .filter_map(|name| {
                Path::new(name)
                    .file_stem()
                    .and_then(OsStr::to_str)
                    .map(str::to_owned)
            })
            .collect();

        Self { names, stems }
    }
}

#[cfg(test)]
mod tests {
    use super::Existing;

    #[test]
    fn indexes_existing_files_by_stem() {
        let existing = Existing::from_names(["Burial - Archangel.flac", "заметка.txt"]);

        assert!(existing.contains_stem("Burial - Archangel"));
        assert!(existing.contains_stem("заметка"));
        assert!(!existing.contains_stem("Burial - Untrue"));
    }

    #[test]
    fn keeps_file_names_as_they_are() {
        let existing = Existing::from_names(["Burial - Archangel.flac"]);

        assert!(existing.names().contains("Burial - Archangel.flac"));
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
