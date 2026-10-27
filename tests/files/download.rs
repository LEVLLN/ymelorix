use std::time::Duration;

use anyhow::Context as _;
use rstest::rstest;
use yandex_music::{YandexMusicClient, model::track::Track};

use crate::{
    download::{self, Outcome, Quality},
    track::{Layout, TrackInfo},
    updater::{Destination, Mode},
};

fn offline_http() -> anyhow::Result<reqwest::Client> {
    // Любой сетевой запрос здесь ошибочен и не должен уйти во внешний API.
    Ok(reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all("http://127.0.0.1:0")?)
        .timeout(Duration::from_millis(100))
        .build()?)
}

fn item() -> anyhow::Result<TrackInfo> {
    let model: Track = serde_json::from_value(serde_json::json!({
        "id": "1", "realId": "1", "title": "Archangel.live",
        "artists": [{"name": "Burial"}],
        "albums": [{"title": "Untrue"}]
    }))?;
    Ok(TrackInfo::try_from(&model)?)
}

#[rstest]
#[case::flatten(Layout::Flatten, "Burial - Archangel.live.mp3")]
#[case::structured(Layout::Structured, "Burial/Untrue/Archangel.live.mp3")]
#[tokio::test]
async fn test_update_skips_before_any_network_request(
    #[case] layout: Layout,
    #[case] relative: &str,
) -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join(relative);
    tokio::fs::create_dir_all(path.parent().context("ожидалась директория файла")?).await?;
    tokio::fs::write(&path, b"finished").await?;
    let destination = Destination {
        directory: root.path(),
        mode: Mode::Update,
        layout,
    };
    let existing = destination
        .prepare()
        .await?
        .context("ожидался снимок --update")?;
    let http = offline_http()?;
    let client = YandexMusicClient::from_client(http.clone());
    let context = download::Context {
        client: &client,
        http: &http,
        directory: destination.directory,
        existing: Some(&existing),
        quality: Quality::Lossless,
        layout: destination.layout,
    };
    let track = item()?;

    assert!(matches!(
        download::track(&context, &track).await,
        Ok(Outcome::Skipped { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn test_prepares_structured_directories_before_requesting_a_file() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let http = offline_http()?;
    let client = YandexMusicClient::from_client(http.clone());
    let context = download::Context {
        client: &client,
        http: &http,
        directory: root.path(),
        existing: None,
        quality: Quality::Lossless,
        layout: Layout::Structured,
    };
    let outcome = download::track(&context, &item()?).await;

    assert_eq!(
        (
            matches!(outcome, Err(download::Failure::Track(_))),
            root.path().join("Burial/Untrue").is_dir()
        ),
        (true, true)
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn test_rejects_a_symlink_in_structured_directories() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    std::os::unix::fs::symlink(outside.path(), root.path().join("Burial"))?;
    let http = offline_http()?;
    let client = YandexMusicClient::from_client(http.clone());
    let context = download::Context {
        client: &client,
        http: &http,
        directory: root.path(),
        existing: None,
        quality: Quality::Lossless,
        layout: Layout::Structured,
    };

    assert!(matches!(
        download::track(&context, &item()?).await,
        Err(download::Failure::Fatal(_))
    ));
    assert!(!outside.path().join("Untrue").exists());
    Ok(())
}
