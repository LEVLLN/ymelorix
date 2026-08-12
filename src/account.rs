//! Проверка авторизации через `/account/status`.

use anyhow::{Context as _, bail};
use serde_derive::Deserialize;
use yandex_music::{API_PATH, YandexMusicClient};

use crate::net;

/// Потолок на тело `/account/status`: ответ там на пару килобайт, и мегабайта
/// хватит с запасом. Размер выбирает удалённая сторона — предел наш.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Обёртка ответа API: `{ "invocationInfo": ..., "result": ... }`.
#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    result: T,
}

/// Своя модель вместо `yandex_music::model::account::status::AccountStatus`:
/// в крейте поле `account.child` объявлено обязательным, а API его не отдаёт.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    account: Account,
    plus: Option<Plus>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Account {
    uid: Option<u64>,
    login: Option<String>,
    display_name: Option<String>,
    region: Option<u32>,
    service_available: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Plus {
    has_plus: Option<bool>,
}

/// Возвращает `uid` владельца токена, попутно логируя параметры аккаунта.
///
/// Транспорт и авторизация — крейтовые, а вот `client.get_account_status()`
/// непригоден: его модель требует `account.child`, которого в ответе нет.
///
/// # Errors
///
/// Ошибка, если запрос не ушёл, статус не успешный, тело не разобралось или
/// в ответе нет `uid` (последнее означает анонимный ответ — токен не принят).
pub(crate) async fn fetch_uid(client: &YandexMusicClient) -> anyhow::Result<u64> {
    let response = net::within_deadline(
        "account/status",
        client.inner.get(format!("{API_PATH}account/status")).send(),
    )
    .await?
    .context("запрос /account/status не отправился")?;

    // `error_for_status()` выбрасывает тело, а Яндекс объясняет отказ именно
    // в нём: по форме тела видно, какой слой отверг запрос.
    let status_code = response.status();
    let body = net::read_capped(response, MAX_BODY_BYTES, "account/status").await?;
    let body = String::from_utf8_lossy(&body);
    if !status_code.is_success() {
        tracing::error!(%status_code, %body, "account/status failed");
        bail!("account/status: HTTP {status_code}, тело: {body}");
    }

    let status: ApiResponse<Status> = serde_json::from_str(&body)
        .with_context(|| format!("не удалось разобрать ответ account/status: {body}"))?;
    let Status { account, plus } = status.result;
    let uid = account.uid.context(
        "в ответе account/status нет uid: ответ анонимный, токен не принят — см. README",
    )?;

    tracing::info!(
        uid,
        login = account.login,
        display_name = account.display_name,
        region = account.region,
        service_available = account.service_available,
        has_plus = plus.and_then(|plus| plus.has_plus),
        "authorized"
    );

    Ok(uid)
}
