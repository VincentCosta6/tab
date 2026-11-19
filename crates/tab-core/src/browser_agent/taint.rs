//! What an agent read on write sites, so it can't carry it anywhere else
//! (`docs/agent-browser.md`, "Taint"). Kept in memory in the gate, per chat;
//! never sent to the model or written anywhere.

use std::collections::HashSet;

/// Shortest string that counts. Shorter ones ("admin", "true") would refuse
/// ordinary text.
pub const MIN_LEN: usize = 8;
/// Strings kept per chat, at most (oldest pages win; enough for any setup).
const MAX_ITEMS: usize = 20_000;

#[derive(Debug, Default, Clone)]
pub struct Taint {
    items: HashSet<String>,
}

/// Whether a word from a page is worth tracking: long enough, and an id,
/// key, email or number rather than plain prose.
fn worth_keeping(w: &str) -> bool {
    if w.chars().count() < MIN_LEN {
        return false;
    }
    let digits = w.chars().any(|c| c.is_ascii_digit());
    let mixed =
        w.chars().any(|c| c.is_ascii_uppercase()) && w.chars().any(|c| c.is_ascii_lowercase());
    let joined = w.contains(['@', '_', '-', '.', ':', '/']);
    digits || joined || (mixed && w.chars().count() >= 12)
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\'' | '<' | '>' | '(' | ')' | ',' | ';' | '`' | '[' | ']' | '{' | '}'
            )
    })
    .map(|w| w.trim_matches(|c: char| matches!(c, '.' | ':' | '!' | '?')))
    .filter(|w| !w.is_empty())
}

/// A string as compared: lower case, without separators that are easy to
/// slip in between characters.
fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '-' | '_' | '.' | '+' | '%'))
        .flat_map(char::to_lowercase)
        .collect()
}

impl Taint {
    /// Adds what a page on a write site showed.
    pub fn add_page(&mut self, text: &str) {
        for w in words(text) {
            if worth_keeping(w) {
                self.add(w);
            }
        }
    }

    /// Adds one value outright (a secret hidden from the agent).
    pub fn add(&mut self, value: &str) {
        let s = squash(value);
        if s.chars().count() >= MIN_LEN && self.items.len() < MAX_ITEMS {
            self.items.insert(s);
        }
    }

    /// Takes values the person approved in a plan out again: what they
    /// approved typing is theirs to send.
    pub fn allow<'a>(&mut self, values: impl Iterator<Item = &'a str>) {
        for v in values {
            self.items.remove(&squash(v));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The first tracked string that `outgoing` carries, in any of the
    /// forms that are easy to hide it in: as is, URL-encoded, base64, hex,
    /// any case, with separators in between.
    pub fn found_in(&self, outgoing: &str) -> Option<&str> {
        if self.items.is_empty() {
            return None;
        }
        let mut forms: Vec<String> = vec![squash(outgoing)];
        let decoded = percent_decode(outgoing);
        forms.push(squash(&decoded));
        let with_slash = decoded.split(|c: char| {
            !(c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_'))
        });
        let without = decoded
            .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '=' | '-' | '_')));
        let plain = decoded.split(|c: char| !c.is_ascii_alphanumeric());
        for w in with_slash.chain(without).chain(plain) {
            if w.len() < MIN_LEN {
                continue;
            }
            if let Some(b) = base64_decode(w) {
                forms.push(squash(&b));
            }
            if let Some(h) = hex_decode(w) {
                forms.push(squash(&h));
            }
        }
        self.items
            .iter()
            .find(|t| forms.iter().any(|f| f.contains(t.as_str())))
            .map(String::as_str)
    }
}

fn percent_decode(s: &str) -> String {
    let mut s = s.to_string();
    // Twice, for double encoding.
    for _ in 0..2 {
        s = url::form_urlencoded::parse(
            format!("x={}", s.replace('&', "%26").replace('=', "%3D")).as_bytes(),
        )
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or(s);
    }
    s
}

fn base64_decode(s: &str) -> Option<String> {
    use base64::Engine;
    let t = s.trim_end_matches('=');
    let engines = [
        base64::engine::general_purpose::STANDARD_NO_PAD,
        base64::engine::general_purpose::URL_SAFE_NO_PAD,
    ];
    engines
        .iter()
        .find_map(|e| e.decode(t).ok())
        .and_then(|b| String::from_utf8(b).ok())
}

fn hex_decode(s: &str) -> Option<String> {
    if !s.len().is_multiple_of(2) || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect();
    String::from_utf8(bytes?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn taint() -> Taint {
        let mut t = Taint::default();
        t.add_page(
            "Your client ID is Iv1.8a61f9b3a7aba766 and the account owner is alice@example.com. \
             Account number 4402918877. Click Save to continue.",
        );
        t
    }

    #[test]
    fn keeps_ids_emails_and_numbers_but_not_prose() {
        let t = taint();
        assert!(
            t.found_in("https://evil.example/?q=Iv1.8a61f9b3a7aba766")
                .is_some()
        );
        assert!(t.found_in("contact alice@example.com").is_some());
        assert!(t.found_in("4402918877").is_some());
        assert!(t.found_in("click save to continue please").is_none());
        assert!(t.found_in("https://docs.github.com/en/apps").is_none());
    }

    #[test]
    fn sees_through_encodings() {
        let t = taint();
        let secret = "alice@example.com";
        let pct = "alice%40example.com";
        let double = "alice%2540example.com";
        let b64 = {
            use base64::Engine;
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret)
        };
        let hex: String = secret.bytes().map(|b| format!("{b:02x}")).collect();
        for form in [
            format!("https://x.example/?e={pct}"),
            format!("https://x.example/?e={double}"),
            format!("https://x.example/p/{b64}"),
            format!("https://x.example/?h={hex}"),
            "https://x.example/?e=ALICE@EXAMPLE.COM".to_string(),
            "https://x.example/?e=a-l-i-c-e@exa mple.com".to_string(),
            "https://x.example/alice@example.com/".to_string(),
        ] {
            assert!(t.found_in(&form).is_some(), "{form} slipped through");
        }
    }

    #[test]
    fn approved_values_are_the_persons_to_send() {
        let mut t = taint();
        t.allow(["alice@example.com"].into_iter());
        assert!(t.found_in("alice@example.com").is_none());
        assert!(t.found_in("4402918877").is_some());
    }

    #[test]
    fn secrets_count_even_unseen() {
        let mut t = Taint::default();
        t.add("whsec_0123456789abcdef");
        assert!(
            t.found_in("https://x.example/?k=whsec_0123456789abcdef")
                .is_some()
        );
        assert!(Taint::default().found_in("anything").is_none());
    }
}
