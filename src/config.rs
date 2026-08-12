//! Конфигурация из окружения: токен.
//!
//! Заголовок `X-Yandex-Music-Client` не настраивается — всегда крейтовый
//! `DEFAULT_CLIENT_ID`. На допуск к API он не влияет (это самодекларация версии
//! клиента), а переменная только плодила варианты, которые нечем проверить.

use core::fmt;

use anyhow::{Context as _, bail};

const TOKEN_VAR: &str = "YANDEX_TOKEN";

/// OAuth-токен, уже проверенный на непустоту.
///
/// `Debug` намеренно скрывает значение: токен равносилен паролю, а структуры
/// с ним попадают в логи целиком.
#[derive(Clone)]
pub(crate) struct Token(String);

impl Token {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<скрыт>)")
    }
}

#[derive(Debug)]
pub(crate) struct Config {
    pub(crate) token: Token,
}

impl Config {
    /// Читает конфигурацию из окружения.
    ///
    /// # Errors
    ///
    /// Возвращает ошибку, если `YANDEX_TOKEN` не задан, пуст или содержит
    /// символы, недопустимые в HTTP-заголовке.
    pub(crate) fn from_env() -> anyhow::Result<Self> {
        let token = std::env::var(TOKEN_VAR)
            .with_context(|| format!("не задана переменная {TOKEN_VAR}"))?;
        let token = token.trim();
        if token.is_empty() {
            bail!("переменная {TOKEN_VAR} пуста: как получить токен — см. README");
        }
        ensure_header_safe(token, TOKEN_VAR)?;

        Ok(Self {
            token: Token(token.to_owned()),
        })
    }
}

/// Токен уезжает в HTTP-заголовок, а туда пролезают только видимые ASCII-символы.
/// Ловим это здесь, чтобы вместо `InvalidHeaderValue` из недр клиента пользователь
/// увидел, что именно чинить.
fn ensure_header_safe(value: &str, variable: &str) -> anyhow::Result<()> {
    match value.chars().find(|symbol| !is_header_safe(*symbol)) {
        None => Ok(()),
        Some(symbol) => bail!(
            "переменная {variable} содержит недопустимый символ {symbol:?}: значение уходит \
             в HTTP-заголовок, где разрешены только видимые символы ASCII. Чаще всего это \
             перенос строки, кавычки или кириллица, попавшие при копировании — \
             скопируйте только само значение, без кавычек и переносов"
        ),
    }
}

fn is_header_safe(symbol: char) -> bool {
    symbol.is_ascii_graphic() || symbol == ' '
}
