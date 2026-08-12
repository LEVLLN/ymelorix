//! Сценарий «загрузить избранное»: скачать все любимые треки в директорию.

use yandex_music::YandexMusicClient;

use crate::{
    account,
    commands::batch,
    download::{self, Quality},
    library, net, output,
    updater::Destination,
};

/// Проходит сценарий целиком: директория → авторизация → лайки → скачивание.
///
/// # Errors
///
/// Ошибка, если директорию не подготовить, токен не принят, лайки не получены
/// или выгрузка не дошла до конца — подробности в [`batch::download_all`].
pub(crate) async fn run(
    client: &YandexMusicClient,
    destination: &Destination<'_>,
    quality: Quality,
) -> anyhow::Result<()> {
    let existing = destination.prepare().await?;

    let uid = account::fetch_uid(client).await?;
    let tracks = library::liked_tracks(client, uid).await?;

    output::progress(&format!(
        "Избранных треков: {}, качаю в {}",
        tracks.len(),
        destination.directory.display()
    ));
    if let Some(existing) = existing.as_ref() {
        output::progress(&format!(
            "В директории уже {} файлов — скачаю только недостающее",
            existing.names().len()
        ));
    }

    let http = net::file_client()?;
    let context = download::Context {
        client,
        http: &http,
        directory: destination.directory,
        existing: existing.as_ref(),
        quality,
    };

    batch::download_all(&context, &tracks).await
}
