//! Сетевые примитивы: дедлайны на вызовы API и клиент для файлов.
//!
//! Ни `reqwest`, ни клиент крейта `yandex-music` не ставят таймаутов по
//! умолчанию: в конфигурации `reqwest` поля `timeout`, `read_timeout` и
//! `connect_timeout` — `None`, а билдер крейта задаёт только заголовки. Сервер,
//! который принял соединение и замолчал, без явного срока вешает процесс
//! навсегда — посреди выгрузки и без единой строчки в выводе.

use core::{future::Future, time::Duration};

use anyhow::{Context as _, bail};
use reqwest::header::{HeaderMap, RETRY_AFTER};

/// Дедлайн на один вызов API.
///
/// Ручки Музыки отвечают за доли секунды; тридцать секунд — это уже отказ,
/// а не медленный ответ.
const API_DEADLINE: Duration = Duration::from_secs(30);

/// Сколько ждать установления соединения с раздающим хостом.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Допустимая пауза между байтами тела — не дедлайн на файл целиком.
///
/// Общий срок здесь был бы неверен: lossless-трек на медленном канале
/// качается законно долго, и обрывать его нельзя. Молчание же в тридцать
/// секунд посреди тела означает, что соединение мертво.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Предел цепочки редиректов при скачивании файла.
///
/// Раздача живёт на CDN и один-два перехода делает штатно. Ограничение важно
/// именно потому, что ссылку выдаёт сервер: клиент не должен ходить по
/// произвольно длинной цепочке чужих хостов.
const MAX_REDIRECTS: usize = 3;

/// Дольше этого не ждём между попытками.
///
/// Минута — граница между «сервер занят» и «сервер не хочет вас видеть».
/// Просьбу подождать дольше уважать нечем: держать процесс — и, в cron,
/// блокировку — в ожидании получаса хуже, чем честно остановиться и прийти
/// в следующий раз.
pub(crate) const MAX_WAIT: Duration = Duration::from_mins(1);

/// Сколько раз пробовать вызов API, наткнувшийся на отказ.
///
/// Три попытки с паузами 1 и 2 с переживают короткий троттлинг, но не
/// превращаются в шторм повторов: сам по себе он и есть отказ.
pub(crate) const API_ATTEMPTS: u32 = 3;

/// Что делать после отказа.
///
/// Типизированный ответ, а не `Option<Duration>`: вызывающий на вариантах
/// **матчится**, и «повторять не надо» не должно выглядеть как «пауза нулевая».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Retry {
    /// Подождать столько и повторить.
    After(Duration),
    /// Попытки кончились или ждать пришлось бы слишком долго.
    GiveUp,
}

/// Пауза перед попыткой номер `attempt` (нумерация с нуля): 1, 2, 4 … секунды.
///
/// Удвоение, а не постоянная пауза: если сервер ограничивает темп, повторы
/// с одинаковым интервалом ложатся ровно тем же темпом, который и вызвал отказ.
fn backoff(attempt: u32) -> Duration {
    let seconds = 2_u64.checked_pow(attempt).unwrap_or(u64::MAX);

    Duration::from_secs(seconds).min(MAX_WAIT)
}

/// Решает, повторять ли вызов и сколько ждать перед повтором.
///
/// Просьба сервера (`asked`) важнее нашей лестницы задержек: он один знает,
/// когда квота отпустит. Но и она не безусловна — просьба подождать дольше
/// [`MAX_WAIT`] означает отказ, а не паузу.
pub(crate) fn next_attempt(attempt: u32, attempts: u32, asked: Option<Duration>) -> Retry {
    let wait = asked.unwrap_or_else(|| backoff(attempt));

    if attempt.saturating_add(1) >= attempts || wait > MAX_WAIT {
        Retry::GiveUp
    } else {
        Retry::After(wait)
    }
}

/// Сколько сервер просит подождать, если сказал это заголовком `Retry-After`.
///
/// Понимается только форма «столько-то секунд». Вторая допустимая форма — дата
/// в формате HTTP — требует часов и разбора даты, а Музыка её не присылает;
/// непонятый заголовок означает «сервер не назвал срок», и в дело идёт [`backoff`].
pub(crate) fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Повторяет вызов API, пока он не удастся или не кончатся попытки.
///
/// Повтор безопасен потому, что повторяются **только чтения**: и список лайков,
/// и данные треков ничего не меняют, поэтому лишний запрос не может задвоить
/// эффект. Заголовков здесь нет: вызовы идут через крейт `yandex-music`, а он
/// не отдаёт ни ответа, ни его статуса — поэтому `Retry-After` не виден и
/// причина отказа неизвестна. Отсюда и малое число попыток: повторяется в том
/// числе то, что повторять бессмысленно.
///
/// # Errors
///
/// Ошибка последней попытки, если удачной не случилось.
pub(crate) async fn retrying<T, F, Fut>(what: &str, attempts: u32, mut work: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    let mut attempt = 0_u32;
    loop {
        let error = match work().await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };

        match next_attempt(attempt, attempts, None) {
            Retry::GiveUp => return Err(error),
            Retry::After(wait) => {
                tracing::warn!(
                    what,
                    attempt = attempt.saturating_add(1),
                    of = attempts,
                    wait_secs = wait.as_secs(),
                    error = format!("{error:#}"),
                    "повтор после отказа"
                );
                tokio::time::sleep(wait).await;
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

/// Клиент для скачивания файлов — **без заголовков авторизации**.
///
/// Файлы отдаёт CDN, и OAuth-токену там делать нечего. Именно поэтому же
/// следование редиректам безопасно: по цепочке не едет ни один секрет.
///
/// # Errors
///
/// Ошибка, если в системе не поднялся TLS.
pub(crate) fn file_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
        .build()
        .context(
            "не удалось создать HTTP-клиент для файлов: не поднялся TLS. \
             Проверьте, что в системе установлены корневые сертификаты \
             (пакет ca-certificates)",
        )
}

/// Ограничивает вызов API сроком: без него ожидание ничем не ограничено.
///
/// # Errors
///
/// Ошибка, если за отведённое время ответа не пришло. `what` попадает в текст,
/// поэтому называйте операцию, а не функцию: «список лайков», а не `get_liked`.
pub(crate) async fn within_deadline<T>(
    what: &str,
    work: impl Future<Output = T>,
) -> anyhow::Result<T> {
    tokio::time::timeout(API_DEADLINE, work)
        .await
        .with_context(|| format!("{what}: ответа нет дольше {} с", API_DEADLINE.as_secs()))
}

/// Читает тело ответа, не давая ему вырасти больше `limit` байт.
///
/// `text()` и `bytes()` буферизуют столько, сколько пришлёт сервер: размер
/// ответа выбирает удалённая сторона, а память тратится наша.
///
/// # Errors
///
/// Ошибка, если чтение оборвалось или тело превысило предел.
pub(crate) async fn read_capped(
    mut response: reqwest::Response,
    limit: usize,
    what: &str,
) -> anyhow::Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        // URL уходит из ошибки: он может быть подписанным, а ошибки печатаются.
        .map_err(reqwest::Error::without_url)
        .with_context(|| format!("{what}: тело ответа оборвалось"))?
    {
        if body.len() + chunk.len() > limit {
            bail!("{what}: ответ больше {limit} байт, читать дальше не буду");
        }
        body.extend_from_slice(&chunk);
    }

    Ok(body)
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};
    use rstest::rstest;

    use super::{MAX_WAIT, Retry, backoff, next_attempt, retry_after};

    #[rstest]
    #[case::first(0, 1)]
    #[case::second(1, 2)]
    #[case::third(2, 4)]
    #[case::eighth(7, 60)]
    #[case::no_overflow_on_absurd_attempt(u32::MAX, 60)]
    fn doubles_pause_up_to_the_ceiling(#[case] attempt: u32, #[case] expected_secs: u64) {
        assert_eq!(backoff(attempt), Duration::from_secs(expected_secs));
    }

    #[rstest]
    #[case::waits_before_the_second_attempt(0, 3, None, Retry::After(Duration::from_secs(1)))]
    #[case::waits_before_the_third_attempt(1, 3, None, Retry::After(Duration::from_secs(2)))]
    #[case::attempts_exhausted(2, 3, None, Retry::GiveUp)]
    #[case::single_attempt_never_repeats(0, 1, None, Retry::GiveUp)]
    #[case::obeys_the_server(
        0,
        3,
        Some(Duration::from_secs(7)),
        Retry::After(Duration::from_secs(7))
    )]
    #[case::server_asks_longer_than_we_wait(0, 3, Some(Duration::from_mins(10)), Retry::GiveUp)]
    fn decides_whether_to_repeat(
        #[case] attempt: u32,
        #[case] attempts: u32,
        #[case] asked: Option<Duration>,
        #[case] expected: Retry,
    ) {
        assert_eq!(next_attempt(attempt, attempts, asked), expected);
    }

    #[rstest]
    #[case::seconds("30", Some(Duration::from_secs(30)))]
    #[case::padded(" 30 ", Some(Duration::from_secs(30)))]
    #[case::zero("0", Some(Duration::ZERO))]
    #[case::http_date_not_supported("Wed, 21 Oct 2015 07:28:00 GMT", None)]
    #[case::garbage("soon", None)]
    #[case::negative("-5", None)]
    fn reads_retry_after(#[case] value: &'static str, #[case] expected: Option<Duration>) {
        let mut headers = HeaderMap::new();
        let _no_previous_value = headers.insert(RETRY_AFTER, HeaderValue::from_static(value));

        assert_eq!(retry_after(&headers), expected);
    }

    #[test]
    fn absent_retry_after_is_not_a_pause() {
        assert_eq!(retry_after(&HeaderMap::new()), None);
    }

    #[test]
    fn ceiling_is_a_minute() {
        assert_eq!(MAX_WAIT, Duration::from_mins(1));
    }
}
