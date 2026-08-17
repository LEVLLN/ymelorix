//! Скачивание файлов треков на диск.

use core::fmt;
use std::{
    collections::HashSet,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, anyhow};
use tokio::io::AsyncWriteExt as _;
use yandex_music::{
    YandexMusicClient, api::track::get_file_info::GetFileInfoOptions,
    model::info::file_info::Quality as ApiQuality,
};

use crate::{net, tags, track::TrackInfo, updater::Existing};

/// Какого качества просить файл.
///
/// Своя копия крейтового `Quality`: чужой тип не должен ходить по коду и не
/// может быть аргументом командной строки. Кодек при этом не выбирается — из
/// всех поддерживаемых его подбирает сервер под запрошенное качество.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Quality {
    /// Без потерь: FLAC, самые большие файлы.
    Lossless,
    /// Обычное: сжатие с потерями, экономит трафик и место.
    Normal,
    /// Низкое: для медленной связи.
    Low,
}

impl From<Quality> for ApiQuality {
    fn from(quality: Quality) -> Self {
        match quality {
            Quality::Lossless => Self::Lossless,
            Quality::Normal => Self::Normal,
            Quality::Low => Self::Low,
        }
    }
}

/// Потолок на размер одного файла.
///
/// Длину тела выбирает удалённая сторона, а память и диск тратятся наши. Полчаса
/// lossless — это около трёхсот мегабайт, так что предел взят с запасом и режет
/// только явно ненормальный ответ.
const MAX_TRACK_BYTES: u64 = 512 * 1024 * 1024;

/// Расширение незавершённой загрузки.
///
/// `Path::file_stem` у `X.flac.part` даёт `X.flac`, а не `X`, поэтому остаток
/// оборванной загрузки не будет принят за скачанный трек.
const PART_EXTENSION: &str = "part";

/// Сколько раз просить у раздачи один и тот же файл, пока она отвечает `429`.
const FILE_ATTEMPTS: u32 = 3;

/// Потолок на начало файла, которое копится в памяти ради тегов.
///
/// Теги пишутся на лету: начало файла придерживается, пока не станет ясно, где
/// кончается заголовок. У файлов Музыки `moov` идёт первым и занимает единицы
/// килобайт, так что потолок режет только раскладку, при которой заголовок
/// лежит **после** звука, — там придержать пришлось бы весь файл.
const MAX_HEAD_BYTES: usize = 8 * 1024 * 1024;

/// Что произошло с треком: файл появился или уже лежал на месте.
#[derive(Debug)]
pub(crate) enum Outcome {
    Downloaded { path: PathBuf, bytes: u64 },
    Skipped { stem: String },
}

impl Outcome {
    /// Стоил ли исход обращения к Музыке.
    ///
    /// Счётчик отказов подряд обнуляется только удачей, за которую заплачен
    /// запрос. Пропуск по `--update` сети не касается: обнулять им счётчик —
    /// значит выключить предохранитель ровно там, где он нужнее всего. На почти
    /// полной директории троттлящий сервер даёт чередование «отказ — пропуск»,
    /// и подряд идущих отказов не набирается никогда, сколько бы их ни было.
    pub(crate) const fn cost_request(&self) -> bool {
        match self {
            Self::Downloaded { .. } => true,
            Self::Skipped { .. } => false,
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Downloaded { path, bytes } => {
                write!(f, "скачан {} ({} КБ)", path.display(), bytes / 1024)
            }
            Self::Skipped { stem } => write!(f, "уже есть {stem}"),
        }
    }
}

/// Отказ на треке.
///
/// Типизированный enum, а не `anyhow::Error`, ровно потому, что вызывающий на
/// вариантах **матчится**: от них зависит, есть ли смысл браться за следующий
/// трек.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Failure {
    /// Не задался этот трек: снят с раздачи, битая ссылка, отказ CDN. Соседние
    /// треки скачаются.
    #[error("трек не скачан")]
    Track(#[source] anyhow::Error),
    /// Не задалось окружение: кончилось место, нет прав, директория исчезла.
    /// Следующий трек упрётся ровно в то же самое.
    #[error("выгрузка невозможна")]
    Fatal(#[source] anyhow::Error),
    /// Раздача просит сбавить темп и не перестала просить после повторов.
    /// Отдельный вариант от [`Failure::Track`] потому, что на нём меняется
    /// поведение вызывающего: ни зеркала, ни следующий трек не помогут — они
    /// живут за той же квотой, и каждый лишний запрос её только удерживает.
    #[error("сервер ограничивает темп запросов")]
    Throttled(#[source] anyhow::Error),
}

/// Всё, что нужно для скачивания, кроме самого трека.
pub(crate) struct Context<'a> {
    pub(crate) client: &'a YandexMusicClient,
    pub(crate) http: &'a reqwest::Client,
    pub(crate) directory: &'a Path,
    /// Снимок директории — или `None`, если сверяться не с чем и качать надо
    /// весь список подряд.
    pub(crate) existing: Option<&'a Existing>,
    pub(crate) quality: Quality,
}

/// Кладёт трек в директорию, повторно не перекачивает.
///
/// # Errors
///
/// [`Failure::Track`], если Музыка не дала ссылку или файл не скачался;
/// [`Failure::Fatal`], если отказала сама директория назначения.
pub(crate) async fn track(context: &Context<'_>, track: &TrackInfo) -> Result<Outcome, Failure> {
    let stem = track.file_stem();
    if context
        .existing
        .is_some_and(|existing| existing.contains_stem(&stem))
    {
        return Ok(Outcome::Skipped { stem });
    }

    let file_info = net::within_deadline(
        &format!("ссылка на файл трека {}", track.id()),
        context.client.get_file_info(
            &GetFileInfoOptions::new(track.id().as_str()).quality(context.quality.into()),
        ),
    )
    .await
    .map_err(Failure::Track)?
    .with_context(|| format!("нет ссылки на файл трека {}", track.id()))
    .map_err(Failure::Track)?;

    let extension = extension(&file_info.codec);
    let path = context.directory.join(format!("{stem}.{extension}"));
    let temporary = context
        .directory
        .join(format!("{stem}.{extension}.{PART_EXTENSION}"));

    // Данные для тегов приехали тем же ответом, что и сам трек: за них не
    // платится ни одного лишнего запроса.
    let meta = track.tags();
    let format = tags::Format::of(extension);

    // Сначала во временный файл, потом переименование. `rename` в пределах
    // директории атомарен: оборванная загрузка не оставит правдоподобный
    // огрызок, который следующий запуск сочтёт скачанным и не тронет никогда.
    let mut last_error = None;
    for url in mirrors(&file_info.url, &file_info.urls) {
        let transfer = Transfer {
            http: context.http,
            url: &url,
            temporary: &temporary,
            expected: file_info.size,
            head: format.map(|format| Head {
                format,
                meta: &meta,
                track,
                buffer: Vec::new(),
            }),
        };

        match fetch(transfer).await {
            Ok(bytes) => {
                tokio::fs::rename(&temporary, &path)
                    .await
                    .map_err(|error| {
                        from_io(
                            error,
                            &format!("не удалось переименовать {}", path.display()),
                        )
                    })?;

                return Ok(Outcome::Downloaded { path, bytes });
            }
            // Зеркала перебирать незачем в обоих случаях: сломанная директория
            // сломает и следующую попытку, а квота у зеркал общая с основной
            // ссылкой — стучаться в них значит удерживать её занятой.
            Err(stop @ (Failure::Fatal(_) | Failure::Throttled(_))) => {
                discard(&temporary).await;
                return Err(stop);
            }
            Err(Failure::Track(reason)) => {
                discard(&temporary).await;
                last_error = Some(reason);
            }
        }
    }

    Err(Failure::Track(last_error.unwrap_or_else(|| {
        anyhow!(
            "Музыка не дала ни одной ссылки на файл трека {}",
            track.id()
        )
    })))
}

/// Куда идти за файлом: основная ссылка, затем зеркала.
///
/// Зеркала перечислены в ответе и обычно включают основную ссылку — повторы
/// отсеиваются, чтобы неудача не считалась дважды.
fn mirrors(primary: &str, alternatives: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();

    core::iter::once(primary)
        .chain(alternatives.iter().map(String::as_str))
        .filter(|url| seen.insert((*url).to_owned()))
        .map(str::to_owned)
        .collect()
}

/// Запрашивает файл, пережидая просьбы сбавить темп.
///
/// Повтор безопасен: `GET` ничего не меняет, а недокачанное тело живёт во
/// временном файле, который на каждой попытке создаётся заново.
///
/// Статус ответа виден именно здесь, потому что запрос наш: файлы идут мимо
/// крейта `yandex-music`, отдельным клиентом. На ручках API он теряется, и там
/// от троттлинга остаётся защищаться счётом неудач подряд.
async fn open(http: &reqwest::Client, url: &str) -> Result<reqwest::Response, Failure> {
    let mut attempt = 0_u32;
    loop {
        let response = http
            .get(url)
            .send()
            .await
            // `Display` у `reqwest::Error` дописывает URL, а прямая ссылка на файл
            // подписана и временна: в stderr и в логах ей делать нечего.
            .map_err(reqwest::Error::without_url)
            .context("не удалось запросить файл")
            .map_err(Failure::Track)?;

        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        if status != reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(Failure::Track(anyhow!("файл не отдан: HTTP {status}")));
        }

        let asked = net::retry_after(response.headers());
        match net::next_attempt(attempt, FILE_ATTEMPTS, asked) {
            net::Retry::After(wait) => {
                tracing::warn!(
                    attempt = attempt.saturating_add(1),
                    of = FILE_ATTEMPTS,
                    wait_secs = wait.as_secs(),
                    by_request = asked.is_some(),
                    "раздача просит сбавить темп"
                );
                tokio::time::sleep(wait).await;
                attempt = attempt.saturating_add(1);
            }
            net::Retry::GiveUp => {
                return Err(Failure::Throttled(match asked {
                    Some(asked) if asked > net::MAX_WAIT => anyhow!(
                        "раздача просит подождать {} с — дольше, чем стоит ждать в одном запуске",
                        asked.as_secs()
                    ),
                    // Срок либо назван и короток, либо не назван вовсе.
                    Some(_) | None => {
                        anyhow!("раздача отвечает HTTP {status} и после {FILE_ATTEMPTS} попыток")
                    }
                }));
            }
        }
    }
}

/// Одна попытка выкачать файл: куда идти, куда класть и что дописать в теги.
struct Transfer<'a> {
    http: &'a reqwest::Client,
    url: &'a str,
    temporary: &'a Path,
    expected: Option<u64>,
    /// `None` — расширение незнакомое, теги не пишем и придерживать нечего.
    head: Option<Head<'a>>,
}

/// Придержанное начало файла.
///
/// Теги дописываются на лету: пока не видно, где кончается заголовок, байты
/// копятся здесь, а не уезжают на диск. Иначе пришлось бы после загрузки
/// перекладывать весь файл целиком, чтобы вставить пару сотен байт в начало.
struct Head<'a> {
    format: tags::Format,
    meta: &'a tags::Meta,
    track: &'a TrackInfo,
    buffer: Vec<u8>,
}

impl Head<'_> {
    /// Принимает очередной кусок и, если решение созрело, отдаёт байты для записи.
    ///
    /// `None` — заголовка ещё не хватает, копим дальше.
    fn feed(&mut self, chunk: &[u8]) -> Option<Vec<u8>> {
        self.buffer.extend_from_slice(chunk);
        self.decide(Ending::More)
    }

    /// Поток кончился, а решение так и не созрело: писать то, что есть.
    fn settle(mut self) -> Vec<u8> {
        match self.decide(Ending::Last) {
            Some(bytes) => bytes,
            None => self.buffer,
        }
    }

    fn decide(&mut self, ending: Ending) -> Option<Vec<u8>> {
        match tags::patch(self.format, &self.buffer, self.meta) {
            tags::Patch::Ready { head, used } => {
                let buffered = core::mem::take(&mut self.buffer);
                Some([head.as_slice(), buffered.get(used..).unwrap_or_default()].concat())
            }
            tags::Patch::Refused(reason) => Some(self.untagged(reason)),
            tags::Patch::More => match ending {
                Ending::Last => Some(self.untagged(tags::Reason::Malformed)),
                Ending::More if self.buffer.len() > MAX_HEAD_BYTES => {
                    Some(self.untagged(tags::Reason::Oversized))
                }
                Ending::More => None,
            },
        }
    }

    /// Теги не записаны — но файл записан.
    ///
    /// Осознанная деградация: трек без тегов лучше отсутствующего трека.
    /// Молчать о ней нельзя, иначе разница между «формат не тот» и «разборщик
    /// сломался» видна только по свойствам файла в плеере.
    fn untagged(&mut self, reason: tags::Reason) -> Vec<u8> {
        tracing::warn!(track = %self.track, %reason, "теги не записаны");
        core::mem::take(&mut self.buffer)
    }
}

/// Кончился ли поток — от этого зависит, ждать ли ещё байт.
#[derive(Debug, Clone, Copy)]
enum Ending {
    More,
    Last,
}

/// Качает тело по ссылке во временный файл и возвращает размер записанного.
///
/// Тело пишется потоком, а не буферизуется целиком: размер выбирает сервер.
/// Исключение — начало файла, которое придерживается ради тегов.
async fn fetch(transfer: Transfer<'_>) -> Result<u64, Failure> {
    let Transfer {
        http,
        url,
        temporary,
        expected,
        mut head,
    } = transfer;

    let mut response = open(http, url).await?;
    let mut file = tokio::fs::File::create(temporary).await.map_err(|error| {
        from_io(
            error,
            &format!("не удалось создать {}", temporary.display()),
        )
    })?;

    let mut received = 0_u64;
    let mut written = 0_u64;
    loop {
        let chunk = response
            .chunk()
            .await
            .map_err(reqwest::Error::without_url)
            .context("загрузка оборвалась")
            .map_err(Failure::Track)?;
        let Some(chunk) = chunk else { break };

        received = received.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if received > MAX_TRACK_BYTES {
            return Err(Failure::Track(anyhow!(
                "файл больше {} МБ — это не похоже на трек",
                MAX_TRACK_BYTES / 1024 / 1024
            )));
        }

        match head.as_mut() {
            None => written = written.saturating_add(put(&mut file, &chunk, temporary).await?),
            Some(pending) => {
                if let Some(bytes) = pending.feed(&chunk) {
                    head = None;
                    written = written.saturating_add(put(&mut file, &bytes, temporary).await?);
                }
            }
        }
    }

    if let Some(pending) = head {
        let bytes = pending.settle();
        written = written.saturating_add(put(&mut file, &bytes, temporary).await?);
    }

    // Сверка с обещанным размером ловит и обрыв, и CDN, вернувший страницу
    // с ошибкой под видом успеха, — иначе HTML лёг бы на диск как `.flac`.
    // Сверяется именно принятое: на диске лежит ещё и тег, которого в обещанном
    // размере нет.
    if let Some(expected) = expected
        && expected != received
    {
        return Err(Failure::Track(anyhow!(
            "файл пришёл не целиком: {received} байт вместо {expected}"
        )));
    }

    // Без `sync_all` переименование может оказаться на диске раньше самих
    // данных: после сбоя питания остался бы файл правильного имени и нулевого
    // содержания — ровно та порча, от которой защищает временное имя.
    file.sync_all().await.map_err(|error| {
        from_io(
            error,
            &format!("не удалось сбросить {}", temporary.display()),
        )
    })?;

    Ok(written)
}

/// Пишет кусок и возвращает, сколько байт легло на диск.
async fn put(file: &mut tokio::fs::File, bytes: &[u8], temporary: &Path) -> Result<u64, Failure> {
    file.write_all(bytes).await.map_err(|error| {
        from_io(
            error,
            &format!("не удалось записать {}", temporary.display()),
        )
    })?;

    Ok(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
}

/// Убирает остаток неудавшейся попытки.
///
/// Отказ игнорируется намеренно: причина неудачи уже в руках у вызывающего, и
/// подменять её жалобой на уборку — значит потерять диагноз.
async fn discard(temporary: &Path) {
    let _ignored_removal = tokio::fs::remove_file(temporary).await;
}

/// Переводит отказ файловой системы в отказ нужного рода.
fn from_io(error: std::io::Error, what: &str) -> Failure {
    let fatal = is_environment_failure(error.kind());
    let reason = anyhow::Error::new(error).context(what.to_owned());

    if fatal {
        Failure::Fatal(reason)
    } else {
        Failure::Track(reason)
    }
}

/// Сломано ли окружение, а не конкретный трек.
///
/// Кончившееся место, отозванные права и исчезнувшая директория одинаково
/// сломают все оставшиеся треки: пройти по ним до конца — значит потратить
/// сотню запросов, чтобы сотню раз узнать одно и то же.
///
/// `_other` — единственный уместный тут `_`: `ErrorKind` чужой и
/// `#[non_exhaustive]`, перечислить его целиком нельзя.
fn is_environment_failure(kind: ErrorKind) -> bool {
    match kind {
        ErrorKind::StorageFull
        | ErrorKind::QuotaExceeded
        | ErrorKind::PermissionDenied
        | ErrorKind::ReadOnlyFilesystem
        | ErrorKind::NotFound
        | ErrorKind::NotADirectory => true,
        _other => false,
    }
}

/// Расширение файла по кодеку из ответа Музыки.
///
/// Кодеки с суффиксом `-mp4` — это звук в контейнере MP4, и правильное для него
/// расширение `.m4a`, а не `.mp4`: последнее и системой, и плеерами читается как
/// видео.
fn extension(codec: &str) -> &str {
    match codec {
        "flac" => "flac",
        "mp3" => "mp3",
        "aac" | "he-aac" | "flac-mp4" | "aac-mp4" | "he-aac-mp4" => "m4a",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use rstest::rstest;

    use std::path::PathBuf;

    use super::{ApiQuality, Outcome, Quality, extension, is_environment_failure, mirrors};

    /// Пропуск не касается сети, и обнулять им счётчик отказов подряд нельзя:
    /// на почти полной директории чередование «отказ — пропуск» иначе гасит
    /// предохранитель насовсем.
    #[rstest]
    #[case::downloaded(
        Outcome::Downloaded { path: PathBuf::from("a.flac"), bytes: 1 },
        true
    )]
    #[case::skipped(Outcome::Skipped { stem: "a".to_owned() }, false)]
    fn tells_which_outcomes_cost_a_request(#[case] outcome: Outcome, #[case] expected: bool) {
        assert_eq!(outcome.cost_request(), expected);
    }

    /// Значения уезжают в подписанный запрос, поэтому важны именно они, а не
    /// имена вариантов: `nq` и `lq` со стороны не угадываются.
    #[rstest]
    #[case::lossless(Quality::Lossless, "lossless")]
    #[case::normal(Quality::Normal, "nq")]
    #[case::low(Quality::Low, "lq")]
    fn maps_quality_to_wire_value(#[case] quality: Quality, #[case] expected: &str) {
        assert_eq!(ApiQuality::from(quality).to_string(), expected);
    }

    #[rstest]
    #[case::flac("flac", "flac")]
    #[case::mp3("mp3", "mp3")]
    #[case::aac("aac", "m4a")]
    #[case::he_aac("he-aac", "m4a")]
    #[case::flac_mp4("flac-mp4", "m4a")]
    #[case::aac_mp4("aac-mp4", "m4a")]
    #[case::he_aac_mp4("he-aac-mp4", "m4a")]
    #[case::unknown_codec_becomes_extension("opus", "opus")]
    fn maps_codec_to_extension(#[case] codec: &str, #[case] expected: &str) {
        assert_eq!(extension(codec), expected);
    }

    #[rstest]
    #[case::storage_full(ErrorKind::StorageFull, true)]
    #[case::quota(ErrorKind::QuotaExceeded, true)]
    #[case::permissions(ErrorKind::PermissionDenied, true)]
    #[case::read_only(ErrorKind::ReadOnlyFilesystem, true)]
    #[case::directory_gone(ErrorKind::NotFound, true)]
    #[case::not_a_directory(ErrorKind::NotADirectory, true)]
    #[case::bad_name(ErrorKind::InvalidFilename, false)]
    #[case::interrupted(ErrorKind::Interrupted, false)]
    #[case::other(ErrorKind::Other, false)]
    fn tells_environment_failure_from_track_failure(
        #[case] kind: ErrorKind,
        #[case] expected: bool,
    ) {
        assert_eq!(is_environment_failure(kind), expected);
    }

    #[test]
    fn puts_primary_url_first_and_drops_repeats() {
        let alternatives = [
            "https://one.example/a".to_owned(),
            "https://two.example/a".to_owned(),
        ];

        assert_eq!(
            mirrors("https://one.example/a", &alternatives),
            vec![
                "https://one.example/a".to_owned(),
                "https://two.example/a".to_owned(),
            ]
        );
    }
}
