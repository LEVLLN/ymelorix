//! Сценарий «загрузить трек по ссылке».

use yandex_music::YandexMusicClient;

use crate::{
    download::{self, Quality},
    library, net, output,
    track::TrackId,
    updater::Destination,
};

/// Разбирает ссылку, забирает данные трека и кладёт файл в директорию.
///
/// # Errors
///
/// Ошибка, если ссылка не разобралась, директорию не подготовить, трека нет или
/// файл не скачался.
pub(crate) async fn run(
    client: &YandexMusicClient,
    url: &str,
    destination: &Destination<'_>,
    quality: Quality,
) -> anyhow::Result<()> {
    let id = TrackId::parse(url)?;
    let existing = destination.prepare().await?;

    let track = library::track_by_id(client, &id).await?;

    let http = net::file_client()?;
    let context = download::Context {
        client,
        http: &http,
        directory: destination.directory,
        existing: existing.as_ref(),
        quality,
    };

    let outcome = download::track(&context, &track).await?;
    output::progress(&format!("{track} — {outcome}"));

    Ok(())
}
