//! Сценарий «вывести избранное»: печать списка любимых треков.

use yandex_music::YandexMusicClient;

use crate::{account, library, output};

/// Проходит сценарий целиком: авторизация → лайки → вывод.
///
/// В stdout идут только треки, счётчик — в stderr: так `list-favorites | grep`
/// работает с данными, а не пополам со служебными строками.
///
/// # Errors
///
/// Ошибка, если токен не принят, не удалось получить лайки или записать вывод.
pub(crate) async fn run(client: &YandexMusicClient) -> anyhow::Result<()> {
    let uid = account::fetch_uid(client).await?;
    let tracks = library::liked_tracks(client, uid).await?;

    output::progress(&format!("Избранных треков: {}", tracks.len()));
    for track in &tracks {
        match output::line(&track.to_string())? {
            output::Pipe::Open => {}
            // `| head` закрыл трубу: печатать больше некому, и это не сбой.
            output::Pipe::Closed => return Ok(()),
        }
    }

    Ok(())
}
