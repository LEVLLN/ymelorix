use anyhow::Context as _;
use clap::Parser as _;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _};
use yandex_music::YandexMusicClient;

use crate::{
    cli::{Cli, Command},
    config::Config,
};

mod account;
mod cli;
mod commands;
mod config;
mod download;
mod library;
mod net;
mod output;
mod source;
mod tags;
mod track;
mod updater;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Разбор аргументов раньше всего: `--help` и `--version` не должны требовать токен.
    let cli = Cli::parse();
    init_tracing();

    let config = Config::from_env()?;
    tracing::info!(
        client_id = yandex_music::DEFAULT_CLIENT_ID,
        "X-Yandex-Music-Client"
    );

    // client_id не задаём: билдер сам подставляет DEFAULT_CLIENT_ID.
    let client = YandexMusicClient::builder(config.token.as_str())
        .build()
        // Значения заголовков уже проверены в `Config::from_env`, так что сюда
        // доходят только системные причины — обычно отсутствующий набор
        // корневых сертификатов.
        .context(
            "не удалось создать HTTP-клиент: не поднялся TLS. \
             Проверьте, что в системе установлены корневые сертификаты \
             (пакет ca-certificates)",
        )?;

    match cli.command {
        Command::DownloadFavorites { target } => {
            commands::download_favorite::run(&client, &target.destination(), target.quality).await
        }
        Command::ListFavorites => commands::list_favorite::run(&client).await,
        Command::DownloadLink { url, target } => {
            commands::download_link::run(&client, &url, &target.destination(), target.quality).await
        }
    }
}

fn init_tracing() {
    tracing_subscriber::registry()
        .with(
            // Таргет событий — имя крейта, `ymelorix`: директива, не совпавшая
            // с ним, молча глушит весь вывод, включая тела ошибок и предупреждения
            // о повторах. Имя крейта в директиве пишется с подчёркиваниями.
            EnvFilter::try_from_default_env().unwrap_or_else(|_not_set| "ymelorix=trace".into()),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_span_list(false),
        )
        .init();
}
