//! Общий цикл выгрузки: список треков → файлы в директории.
//!
//! Не сценарий, а их общая часть. Живёт здесь, потому что предохранители —
//! счётчик отказов подряд, отношение к троттлингу, отчёт о неудачах — обязаны
//! быть одинаковыми у всех команд загрузки: разъехавшись, они разъедутся молча.

use std::collections::HashSet;

use crate::{download, output, track::TrackInfo};

/// Сколько отказов подряд считать поломкой окружения, а не невезением.
///
/// Отказ каждого следующего трека по одной и той же причине — истёкший токен,
/// пропавшая сеть — не лечится продолжением цикла: остаток списка только
/// потратит запросы. Порог, а не разбор кода ответа, потому что крейт
/// `yandex-music` теряет статус, оставляя его лишь текстом внутри сообщения.
const GIVE_UP_AFTER: usize = 5;

/// Качает весь список в подготовленную директорию.
///
/// Ошибка на отдельном треке не прерывает выгрузку: недоступный трек не должен
/// стоить остальной сотни. Неудачи считаются и попадают в итоговую ошибку.
///
/// # Errors
///
/// Ошибка, если сломалось окружение, сервер попросил сбавить темп, отказов
/// подряд накопилось больше [`GIVE_UP_AFTER`] или хотя бы один трек не скачался.
pub(crate) async fn download_all(
    context: &download::Context<'_>,
    tracks: &[TrackInfo],
) -> anyhow::Result<()> {
    let total = tracks.len();
    let mut written = HashSet::new();
    let mut failed = 0_usize;
    let mut in_a_row = 0_usize;

    for (index, track) in tracks.iter().enumerate() {
        let position = index + 1;
        match download::track(context, track).await {
            Ok(outcome) => {
                // Обнуляет счётчик только удача, стоившая запроса: пропуск сети
                // не касается и о её состоянии ничего не говорит.
                if outcome.cost_request() {
                    in_a_row = 0;
                }

                let overwritten = matches!(outcome, download::Outcome::Downloaded { .. })
                    && !written.insert(track.file_stem());
                if overwritten {
                    tracing::warn!(
                        track = %track,
                        stem = track.file_stem(),
                        "перезаписан файл одноимённого трека"
                    );
                }

                let note = if overwritten {
                    " (перезаписал одноимённый трек)"
                } else {
                    ""
                };
                output::progress(&format!("[{position}/{total}] {track} — {outcome}{note}"));
            }
            Err(download::Failure::Fatal(reason)) => {
                return Err(reason.context(format!("выгрузка прервана на {position} из {total}")));
            }
            // Сервер сказал прямым текстом, что запросов слишком много, и не
            // передумал за три попытки с паузами. Остаток списка ничего не
            // скачает, зато продлит ограничение: скачанное уже на диске,
            // и повтор с `--update` продолжит с этого места.
            Err(download::Failure::Throttled(reason)) => {
                return Err(reason.context(format!(
                    "остановился на {position} из {total}; повторите позже — \
                     с `--update` докачается только недостающее"
                )));
            }
            Err(download::Failure::Track(reason)) => {
                failed += 1;
                in_a_row += 1;
                tracing::warn!(track = %track, error = format!("{reason:#}"), "трек не скачан");
                output::progress(&format!(
                    "[{position}/{total}] {track} — не скачан: {reason:#}"
                ));

                if in_a_row >= GIVE_UP_AFTER {
                    return Err(reason.context(format!(
                        "{in_a_row} трека подряд не скачались — дело не в треках; \
                         остановился на {position} из {total}"
                    )));
                }
            }
        }
    }

    if failed > 0 {
        anyhow::bail!("не скачано треков: {failed} из {total}");
    }

    Ok(())
}
