//! Splits a document into pieces small enough to embed and to quote.
//!
//! Pieces follow paragraphs, and each carries the heading it sits under,
//! so a piece found by search still says what it's about.

/// About how long a piece is, in characters.
pub const TARGET: usize = 1200;
/// Most pieces kept from one resource.
pub const MAX_CHUNKS: usize = 2000;

/// One document of a resource: a page, a file, an email.
#[derive(Debug, Clone, PartialEq)]
pub struct Doc {
    pub title: String,
    /// Where it came from, for citations.
    pub source: Option<String>,
    pub text: String,
}

/// One piece of a document.
#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    pub title: String,
    pub source: Option<String>,
    pub text: String,
}

fn heading(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let rest = t.trim_start_matches('#');
    ((1..=6).contains(&(t.len() - rest.len())) && rest.starts_with(' ')).then_some(t)
}

/// Splits one long paragraph at sentence ends, or at spaces, near `max`.
fn split_long(p: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = p.trim();
    while rest.len() > max {
        let mut cut = max;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        let window = &rest[..cut];
        let at = window
            .rfind(". ")
            .filter(|i| *i > max / 2)
            .map(|i| i + 1)
            .or_else(|| window.rfind(char::is_whitespace).filter(|i| *i > max / 2))
            .unwrap_or(cut);
        out.push(rest[..at].trim().to_string());
        rest = rest[at..].trim_start();
    }
    if !rest.is_empty() {
        out.push(rest.to_string());
    }
    out
}

/// The pieces of `docs`, in order, at most [`MAX_CHUNKS`].
pub fn chunk(docs: &[Doc]) -> Vec<Piece> {
    let mut out = Vec::new();
    for doc in docs {
        let mut current = String::new();
        let mut under: Option<String> = None;
        let push = |current: &mut String, out: &mut Vec<Piece>| {
            let t = current.trim();
            if !t.is_empty() {
                out.push(Piece {
                    title: doc.title.clone(),
                    source: doc.source.clone(),
                    text: t.to_string(),
                });
            }
            current.clear();
        };
        let paragraphs = doc.text.split("\n\n").flat_map(|block| {
            // A heading starts its own paragraph even without a blank line.
            let mut parts = Vec::new();
            let mut cur = String::new();
            for line in block.lines() {
                if heading(line).is_some() && !cur.trim().is_empty() {
                    parts.push(std::mem::take(&mut cur));
                }
                cur.push_str(line);
                cur.push('\n');
            }
            parts.push(cur);
            parts
        });
        for para in paragraphs {
            let para = para.trim();
            if para.is_empty() {
                continue;
            }
            let first = para.lines().next().unwrap_or("");
            if let Some(h) = heading(first) {
                // A new section: start a new piece.
                push(&mut current, &mut out);
                under = Some(h.to_string());
            }
            for part in split_long(para, TARGET) {
                if !current.is_empty() && current.len() + part.len() > TARGET {
                    push(&mut current, &mut out);
                }
                if current.is_empty()
                    && let Some(h) = &under
                    && !part.starts_with(h.as_str())
                {
                    current.push_str(h);
                    current.push_str("\n\n");
                }
                current.push_str(&part);
                current.push_str("\n\n");
            }
            if out.len() >= MAX_CHUNKS {
                break;
            }
        }
        push(&mut current, &mut out);
        if out.len() >= MAX_CHUNKS {
            out.truncate(MAX_CHUNKS);
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> Doc {
        Doc {
            title: "Guide".into(),
            source: Some("https://x.dev/guide".into()),
            text: text.into(),
        }
    }

    #[test]
    fn short_documents_are_one_piece() {
        let p = chunk(&[doc("Hello there.\n\nSecond paragraph.")]);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].text, "Hello there.\n\nSecond paragraph.");
        assert_eq!(p[0].source.as_deref(), Some("https://x.dev/guide"));
    }

    #[test]
    fn sections_split_and_carry_their_heading() {
        let long = "Refunds are given within thirty days. ".repeat(60);
        let text = format!("# Intro\nWelcome.\n## Refunds\n{long}\n\n## Shipping\nTwo days.");
        let p = chunk(&[doc(&text)]);
        assert_eq!(p[0].text, "# Intro\nWelcome.");
        let refunds: Vec<_> = p
            .iter()
            .filter(|x| x.text.contains("thirty days"))
            .collect();
        assert!(refunds.len() >= 2, "{p:#?}");
        assert!(refunds.iter().all(|x| x.text.starts_with("## Refunds")));
        assert!(refunds.iter().all(|x| x.text.len() <= TARGET + 40));
        assert_eq!(p.last().unwrap().text, "## Shipping\nTwo days.");
    }

    #[test]
    fn splits_text_with_no_breaks() {
        let p = chunk(&[doc(&"word ".repeat(1000))]);
        assert!(p.len() >= 4);
        assert!(p.iter().all(|x| x.text.len() <= TARGET));
        assert!(chunk(&[doc("  \n\n ")]).is_empty());
    }
}
