//! Фонотека: любимые треки, альбомы и плейлисты.

use anyhow::{Context as _, bail};
use serde_derive::Deserialize;
use yandex_music::{
    API_PATH, YandexMusicClient,
    api::{
        album::get_album::GetAlbumOptions,
        track::{get_liked_tracks::GetLikedTracksOptions, get_tracks::GetTracksOptions},
    },
};

use crate::{
    net,
    source::{AlbumId, PlaylistId},
    track::{TrackId, TrackInfo},
};

/// `POST /tracks` принимает идентификаторы пачками; на сотне работают
/// официальные клиенты.
const BATCH: usize = 100;

/// Потолок на тело ответа плейлиста.
///
/// Тело просят «бедным» (`richTracks=false`), поэтому на трек уходит около
/// сотни байт: восьми мегабайт хватает на плейлист в десятки тысяч треков,
/// а размер ответа выбирает всё-таки удалённая сторона.
const MAX_PLAYLIST_BYTES: usize = 8 * 1024 * 1024;

/// Забирает лайки целиком: идентификаторы плюс данные о треках.
///
/// # Errors
///
/// Ошибка, если не удалось получить список лайков или любую из пачек треков.
pub(crate) async fn liked_tracks(
    client: &YandexMusicClient,
    uid: u64,
) -> anyhow::Result<Vec<TrackInfo>> {
    let library = net::retrying("список лайков", net::API_ATTEMPTS, || async {
        net::within_deadline(
            "список лайков",
            client.get_liked_tracks(&GetLikedTracksOptions::new(uid)),
        )
        .await?
        .context("не удалось получить список лайков")
    })
    .await?;
    let ids: Vec<String> = library
        .tracks
        .iter()
        .map(|track| track.id.clone())
        .collect();
    tracing::info!(count = ids.len(), "liked track ids");

    tracks_by_ids(client, &ids).await
}

/// Треки альбома по порядку, как они идут на диске.
///
/// # Errors
///
/// Ошибка, если запрос не прошёл или альбома с таким номером нет.
pub(crate) async fn album_tracks(
    client: &YandexMusicClient,
    album: &AlbumId,
) -> anyhow::Result<Vec<TrackInfo>> {
    let found = net::retrying("данные альбома", net::API_ATTEMPTS, || async {
        net::within_deadline(
            "данные альбома",
            client.get_album(&GetAlbumOptions::new(album.value()).with_tracks()),
        )
        .await?
        .with_context(|| format!("не удалось получить альбом {}", album.value()))
    })
    .await?;

    // Ручка отвечает `200` и на несуществующий альбом, объясняя отказ полем
    // внутри тела: без этой проверки пустой альбом было бы не отличить от
    // выдуманного.
    if let Some(error) = found.error {
        bail!("альбом {}: {error}", album.value());
    }

    let tracks: Vec<TrackInfo> = found
        .volumes
        .iter()
        .flatten()
        .map(TrackInfo::try_from)
        .collect::<Result<_, _>>()
        .with_context(|| format!("альбом {} отдан в неожиданном виде", album.value()))?;
    if tracks.is_empty() {
        bail!("в альбоме {} нет треков", album.value());
    }

    Ok(tracks)
}

/// Треки плейлиста в порядке плейлиста.
///
/// Запрос идёт мимо крейта, как и `/account/status`, по двум причинам сразу.
/// Ручке нужен владелец, а крейтовый `GetPlaylistOptions` принимает `u64` —
/// логина из ссылки туда не положить. И модель плейлиста в крейте требует
/// десяток полей (`cover`, `owner`, `created`), которых в ответе может не
/// оказаться, — ровно та поломка, что уже случилась с `account.child`.
/// Своя модель берёт из ответа только идентификаторы, а данные треков
/// добираются общим путём — теми же пачками, что и лайки.
///
/// # Errors
///
/// Ошибка, если запрос не прошёл, плейлист не отдан или пришёл пустым при
/// ненулевом счётчике треков.
pub(crate) async fn playlist_tracks(
    client: &YandexMusicClient,
    playlist: &PlaylistId,
) -> anyhow::Result<Vec<TrackInfo>> {
    // Владелец проверен при разборе ссылки: в путь идут только латиница,
    // цифры, дефис, точка и подчёркивание.
    let path = format!(
        "{API_PATH}users/{}/playlists/{}?richTracks=false",
        playlist.owner(),
        playlist.kind()
    );

    let body = net::retrying("плейлист", net::API_ATTEMPTS, || async {
        let response = net::within_deadline("плейлист", client.inner.get(&path).send())
            .await?
            .context("запрос плейлиста не отправился")?;

        // Тело важнее статуса: в нём Яндекс объясняет отказ — как и в
        // `/account/status`, поэтому `error_for_status` здесь не годится.
        let status_code = response.status();
        let body = net::read_capped(response, MAX_PLAYLIST_BYTES, "плейлист").await?;
        let body = String::from_utf8_lossy(&body).into_owned();
        if !status_code.is_success() {
            tracing::error!(%status_code, %body, "playlist failed");
            bail!("плейлист не отдан: HTTP {status_code}, тело: {body}");
        }

        Ok(body)
    })
    .await?;

    let response: Envelope<Playlist> = serde_json::from_str(&body)
        .with_context(|| format!("не удалось разобрать ответ плейлиста: {body}"))?;
    let Playlist {
        title,
        track_count,
        tracks,
    } = response.result;

    if tracks.is_empty() {
        match track_count {
            Some(0) | None => bail!("в плейлисте нет треков"),
            Some(count) => bail!(
                "плейлист обещает {count} треков, но не отдал ни одного — \
                 похоже, ручка ответила иначе, чем ожидалось"
            ),
        }
    }
    let received = u32::try_from(tracks.len()).unwrap_or(u32::MAX);
    if track_count.is_some_and(|promised| promised > received) {
        tracing::warn!(
            promised = track_count,
            received,
            "плейлист отдан не целиком"
        );
    }

    tracing::info!(title, count = tracks.len(), "playlist track ids");

    let ids: Vec<String> = tracks
        .into_iter()
        .map(|track| track.id.into_string())
        .collect();

    tracks_by_ids(client, &ids).await
}

/// Добирает данные треков по идентификаторам — пачками, как это делают
/// официальные клиенты.
async fn tracks_by_ids(
    client: &YandexMusicClient,
    ids: &[String],
) -> anyhow::Result<Vec<TrackInfo>> {
    let mut tracks = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(BATCH) {
        let batch = net::retrying("данные треков", net::API_ATTEMPTS, || async {
            net::within_deadline(
                "данные треков",
                client.get_tracks(&GetTracksOptions::new(chunk.to_vec())),
            )
            .await?
            .with_context(|| format!("не удалось получить данные {} треков", chunk.len()))
        })
        .await?;
        for track in &batch {
            tracks.push(
                TrackInfo::try_from(track)
                    .with_context(|| format!("трек {} отдан в неожиданном виде", track.id))?,
            );
        }
    }

    Ok(tracks)
}

/// Обёртка ответа API: `{ "invocationInfo": ..., "result": ... }`.
#[derive(Debug, Deserialize)]
struct Envelope<T> {
    result: T,
}

/// Из плейлиста берутся только идентификаторы: остальное приедет с данными
/// треков и не будет зависеть от того, что ручка решит положить в ответ.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Playlist {
    title: Option<String>,
    track_count: Option<u32>,
    #[serde(default)]
    tracks: Vec<PlaylistEntry>,
}

#[derive(Debug, Deserialize)]
struct PlaylistEntry {
    id: EntryId,
}

/// Идентификатор трека приезжает то числом, то строкой — в зависимости от
/// ручки и настроения. Обе формы одинаково допустимы, дальше он строка.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum EntryId {
    Text(String),
    Number(u64),
}

impl EntryId {
    fn into_string(self) -> String {
        match self {
            Self::Text(id) => id,
            Self::Number(id) => id.to_string(),
        }
    }
}

/// Данные одного трека по идентификатору.
///
/// # Errors
///
/// Ошибка, если запрос не прошёл или трека с таким идентификатором нет.
pub(crate) async fn track_by_id(
    client: &YandexMusicClient,
    id: &TrackId,
) -> anyhow::Result<TrackInfo> {
    let tracks = net::retrying("данные трека", net::API_ATTEMPTS, || async {
        net::within_deadline(
            "данные трека",
            client.get_tracks(&GetTracksOptions::new(vec![id.as_str().to_owned()])),
        )
        .await?
        .with_context(|| format!("не удалось получить данные трека {id}"))
    })
    .await?;

    match tracks.as_slice() {
        [] => bail!("трек {id} не найден"),
        [track, ..] => TrackInfo::try_from(track)
            .with_context(|| format!("трек {id} отдан в неожиданном виде")),
    }
}
