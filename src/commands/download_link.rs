//! Сценарий «загрузить альбом или плейлист по ссылке».

use yandex_music::YandexMusicClient;

use crate::{
    commands::batch,
    download::{self, Quality},
    library, net, output,
    source::Source,
    updater::Destination,
};

/// Разбирает ссылку, забирает список треков и качает его целиком.
///
/// Авторизацию отдельным запросом здесь не проверяем, в отличие от избранного:
/// там `uid` нужен самой ручке лайков, а альбом и плейлист адресуются ссылкой.
/// Непринятый токен всё равно всплывёт на первом же запросе.
///
/// # Errors
///
/// Ошибка, если ссылка не разобралась, директорию не подготовить, подборка не
/// получена или выгрузка не дошла до конца — подробности в
/// [`batch::download_all`].
pub(crate) async fn run(
    client: &YandexMusicClient,
    url: &str,
    destination: &Destination<'_>,
    quality: Quality,
) -> anyhow::Result<()> {
    let source = Source::parse(url)?;
    let existing = destination.prepare().await?;

    let tracks = match &source {
        Source::Album(album) => library::album_tracks(client, album).await?,
        Source::Playlist(playlist) => library::playlist_tracks(client, playlist).await?,
    };

    output::progress(&format!(
        "{source}: треков {}, качаю в {}",
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
