use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use clap::Parser as _;
use rstest::rstest;

use crate::{
    cli::{Cli, Command},
    track::Layout,
    updater::{Destination, Existing, Mode},
};

#[rstest]
#[case::favorites("download-favorites")]
#[case::link("download-link")]
#[tokio::test]
async fn test_structured_update_reads_nested_files(#[case] command: &str) -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let album = root.path().join("Burial/Untrue");
    tokio::fs::create_dir_all(&album).await?;
    tokio::fs::write(album.join("Archangel.live.flac"), b"finished").await?;
    let mut args = vec!["ymelorix", command];
    if command == "download-link" {
        args.push("33898323");
    }
    args.extend(["--files-layout", "structured", "--update"]);
    let mut target = match Cli::try_parse_from(args)?.command {
        Command::DownloadFavorites { target } | Command::DownloadLink { target, .. } => target,
        Command::ListFavorites => anyhow::bail!("ожидалась команда загрузки"),
    };
    target.path = root.path().to_owned();
    let destination: Destination<'_> = target.destination();
    let existing = destination
        .prepare()
        .await?
        .context("ожидался снимок --update")?;

    assert!(existing.contains_stem(Path::new("Burial/Untrue/Archangel.live")));
    Ok(())
}

#[rstest]
#[case::flatten(Layout::Flatten, &["root.flac"])]
#[case::structured(Layout::Structured, &["root.flac", "Burial/artist.mp3", "Burial/Untrue/Archangel.live.flac"])]
#[tokio::test]
async fn test_snapshot_is_bounded_by_layout_and_excludes_directories(
    #[case] layout: Layout,
    #[case] expected: &[&str],
) -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    tokio::fs::create_dir_all(root.path().join("Burial/Untrue/deeper")).await?;
    tokio::fs::create_dir(root.path().join("not-a-track.flac")).await?;
    for relative in [
        "root.flac",
        "Burial/artist.mp3",
        "Burial/Untrue/Archangel.live.flac",
        "Burial/Untrue/deeper/ignored.flac",
    ] {
        tokio::fs::write(root.path().join(relative), b"finished").await?;
    }
    let existing = Existing::read(root.path(), layout).await?;

    assert_eq!(
        existing.names().iter().cloned().collect::<BTreeSet<_>>(),
        expected.iter().map(PathBuf::from).collect()
    );
    Ok(())
}

#[rstest]
#[case::flatten(Layout::Flatten)]
#[case::structured(Layout::Structured)]
#[tokio::test]
async fn test_full_mode_creates_destination_without_snapshot(
    #[case] layout: Layout,
) -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = root.path().join("new");
    let destination = Destination {
        directory: &directory,
        mode: Mode::Full,
        layout,
    };

    assert_eq!(
        (destination.prepare().await?.is_none(), directory.is_dir()),
        (true, true)
    );
    Ok(())
}

#[tokio::test]
async fn test_snapshot_does_not_change_during_a_run() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let album = root.path().join("Burial/Untrue");
    tokio::fs::create_dir_all(&album).await?;
    let existing = Existing::read(root.path(), Layout::Structured).await?;
    tokio::fs::write(album.join("Archangel.flac"), b"finished").await?;

    assert!(!existing.contains_stem("Burial/Untrue/Archangel"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn test_snapshot_does_not_follow_symlinks() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    tokio::fs::write(outside.path().join("outside.flac"), b"outside").await?;
    std::os::unix::fs::symlink(outside.path(), root.path().join("Burial"))?;
    std::os::unix::fs::symlink(
        outside.path().join("outside.flac"),
        root.path().join("linked.flac"),
    )?;
    std::os::unix::fs::symlink(root.path(), root.path().join("loop"))?;

    assert!(
        Existing::read(root.path(), Layout::Structured)
            .await?
            .names()
            .is_empty()
    );
    Ok(())
}
