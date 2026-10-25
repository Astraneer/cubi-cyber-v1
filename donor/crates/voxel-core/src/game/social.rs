//! Люди в мультиплеере (G2, mp/ui.md §1, §4): правила ников, чистка и частота сообщений чата, сглаживание пинга для
//! Tab. Чистые функции без состояния игры — их зовут и сервер (server.rs), и клиент (client.rs); так игрок видит те же
//! границы, что проверяет сервер. Виды сообщений чата — `proto::CHAT_*`; команды — game/commands.rs.

/// Ник: 3–16 знаков (символов Unicode, не байт).
pub(crate) const NICK_MIN: usize = 3;
pub(crate) const NICK_MAX: usize = 16;
/// Сообщение чата: не больше стольких символов (длиннее — обрезается).
pub(crate) const CHAT_MAX: usize = 256;
/// Корзина сообщений: всплеск до CHAT_BURST подряд, дальше одно в CHAT_EVERY_TICKS тиков (2 с). Как счётчик флуда MC, но
/// без кика: лишнее просто не отправляется, игрок видит строку-отказ.
pub(crate) const CHAT_BURST: f32 = 4.0;
pub(crate) const CHAT_EVERY_TICKS: f32 = 40.0;
/// Текст отказа по частоте (серым у отправителя).
pub(crate) const CHAT_TOO_FAST: &str = "Слишком часто — сообщение не отправлено";
/// Подсказка в отказе по нику.
pub(crate) const NICK_RULE: &str = "ник: 3–16 символов — латиница, кириллица, цифры и «_»";

/// Знак ника: латиница, кириллица (U+0400–U+04FF), цифра или «_».
fn nick_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || ('\u{400}'..='\u{4ff}').contains(&c)
}

/// Ник годится: 3–16 знаков из `nick_char`. Err — текст правила для игрока.
pub(crate) fn nick_ok(nick: &str) -> Result<(), &'static str> {
    let n = nick.chars().count();
    if (NICK_MIN..=NICK_MAX).contains(&n) && nick.chars().all(nick_char) {
        Ok(())
    } else {
        Err(NICK_RULE)
    }
}

/// Текст чата как его примет сервер: управляющие знаки — прочь, пробелы и переводы строк — в один пробел, края
/// обрезаны, длина — не больше CHAT_MAX символов. Пустой результат — сообщения нет.
pub(crate) fn clean_chat(text: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if c.is_control() {
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    match out.char_indices().nth(CHAT_MAX) {
        Some((i, _)) => {
            out.truncate(i);
            out.trim_end().to_string()
        }
        None => out,
    }
}

/// Корзина сообщений игрока: токены пополняются со временем сервера (в тиках), одно сообщение — один токен.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ChatBucket {
    tokens: f32,
    at: u64,
}

impl ChatBucket {
    pub(crate) fn new(now: u64) -> ChatBucket {
        ChatBucket { tokens: CHAT_BURST, at: now }
    }
    /// Можно ли отправить сейчас (тогда токен списан).
    pub(crate) fn take(&mut self, now: u64) -> bool {
        let dt = now.saturating_sub(self.at) as f32;
        self.at = now;
        self.tokens = (self.tokens + dt / CHAT_EVERY_TICKS).min(CHAT_BURST);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Пинг для Tab (mp/design.md §3.8): `(старое·3 + новое) / 4`; первого замера нет — он и есть значение.
pub(crate) fn smooth_ping(old: Option<u32>, new: u32) -> u32 {
    old.map_or(new, |o| (o * 3 + new) / 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nick_rules() {
        for ok in ["Bot", "Вася", "Игрок123", "Вася_1", "abcdefghijklmnop", "ёЁё", "Їжак"] {
            assert!(nick_ok(ok).is_ok(), "{ok}");
        }
        for bad in ["", "A", "Ab", "abcdefghijklmnopq", "Игрок 1", "a/b", "ник\n", "名前名前", "A-B-C", "ab\u{0}c", "😀😀😀"] {
            assert!(nick_ok(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn chat_clean_text() {
        assert_eq!(clean_chat("  привет \n\t мир  "), "привет мир");
        assert_eq!(clean_chat("a\u{7}b\u{1b}[31mc"), "ab[31mc");
        assert_eq!(clean_chat(" \n \t"), "");
        let long = "я".repeat(300);
        assert_eq!(clean_chat(&long).chars().count(), CHAT_MAX);
        // Обрезка по символам, а не по байтам; хвостовой пробел после обрезки — прочь.
        let s = format!("{} {}", "а".repeat(CHAT_MAX - 1), "б".repeat(10));
        let c = clean_chat(&s);
        assert_eq!(c.chars().count(), CHAT_MAX - 1);
        assert!(!c.ends_with(' '));
    }

    #[test]
    fn chat_bucket_burst_and_refill() {
        let mut b = ChatBucket::new(100);
        assert!((0..4).all(|_| b.take(100)), "всплеск до CHAT_BURST");
        assert!(!b.take(100), "пятое подряд — нет");
        assert!(!b.take(120), "через секунду токена ещё нет");
        assert!(b.take(140), "через 2 с — один");
        assert!(!b.take(140));
        // Долгая тишина не копит больше всплеска.
        assert!((0..4).all(|_| b.take(10_000)));
        assert!(!b.take(10_000));
    }

    #[test]
    fn ping_smoothing() {
        assert_eq!(smooth_ping(None, 80), 80);
        assert_eq!(smooth_ping(Some(80), 40), 70);
        assert_eq!(smooth_ping(Some(0), 100), 25);
    }
}
