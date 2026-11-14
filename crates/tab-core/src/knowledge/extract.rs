//! Plain text out of what resources hold: web pages, PDFs, Word and Excel
//! files, markdown and other text.

use std::io::{Cursor, Read};

use quick_xml::events::Event;

/// File types a project's Files can hold, by extension.
pub const FILE_TYPES: &[&str] = &[
    "pdf", "docx", "xlsx", "md", "mdx", "markdown", "txt", "text", "csv", "tsv", "json", "html",
    "htm", "rst", "org", "yaml", "yml", "toml", "xml", "log",
];

/// Largest text kept from one file or page; the rest is cut.
pub const MAX_TEXT: usize = 2_000_000;

fn ext(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// Whether tab can read a file with this name.
pub fn supported(name: &str) -> bool {
    FILE_TYPES.contains(&ext(name).as_str())
}

/// The text of a file, by its name and contents.
pub fn file_text(name: &str, bytes: &[u8]) -> anyhow::Result<String> {
    let text = match ext(name).as_str() {
        _ if bytes.starts_with(b"%PDF") => pdf(bytes)?,
        "pdf" => anyhow::bail!("{name} isn't a PDF inside"),
        "docx" => docx(bytes)?,
        "xlsx" => xlsx(bytes)?,
        "html" | "htm" => html(&String::from_utf8_lossy(bytes)).text,
        e if FILE_TYPES.contains(&e) => String::from_utf8_lossy(bytes).into_owned(),
        e => anyhow::bail!("tab can't read .{e} files yet"),
    };
    Ok(cut(text))
}

fn cut(mut s: String) -> String {
    if s.len() > MAX_TEXT {
        let mut end = MAX_TEXT;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

fn pdf(bytes: &[u8]) -> anyhow::Result<String> {
    // The parser panics on some malformed files; that's a failed file, not a crash.
    let owned = bytes.to_vec();
    match std::panic::catch_unwind(move || pdf_extract::extract_text_from_mem(&owned)) {
        Ok(Ok(t)) if t.trim().is_empty() => {
            anyhow::bail!("this PDF has no text in it (a scan?)")
        }
        Ok(Ok(t)) => Ok(t),
        Ok(Err(e)) => anyhow::bail!("couldn't read this PDF: {e}"),
        Err(_) => anyhow::bail!("couldn't read this PDF"),
    }
}

fn zip_entry(
    zip: &mut zip::ZipArchive<Cursor<&[u8]>>,
    name: &str,
) -> anyhow::Result<Option<String>> {
    let mut f = match zip.by_name(name) {
        Ok(f) => f,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut s = String::new();
    // Unzipped size is bounded: a zip bomb stops at MAX_TEXT * 8.
    (&mut f)
        .take((MAX_TEXT * 8) as u64)
        .read_to_string(&mut s)?;
    Ok(Some(s))
}

/// The local part of an XML name (`w:t` → `t`).
fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// The character an entity or character reference (`&amp;`, `&#38;`) stands for.
fn reference(g: &quick_xml::events::BytesRef<'_>) -> Option<char> {
    if g.is_char_ref() {
        return g.resolve_char_ref().ok().flatten();
    }
    entity(&g.xml10_content())
}

/// A Word document's paragraphs, in order.
fn docx(bytes: &[u8]) -> anyhow::Result<String> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))?;
    let xml = zip_entry(&mut zip, "word/document.xml")?
        .ok_or_else(|| anyhow::anyhow!("not a Word document"))?;
    let mut r = quick_xml::Reader::from_str(&xml);
    let mut out = String::new();
    let mut in_text = false;
    loop {
        match r.read_event()? {
            Event::Start(e) if local(e.name().as_ref()) == "t" => in_text = true,
            Event::End(e) => match local(e.name().as_ref()) {
                "t" => in_text = false,
                "p" => out.push('\n'),
                "tc" => out.push_str(" | "),
                _ => {}
            },
            Event::Empty(e) => match local(e.name().as_ref()) {
                "tab" => out.push('\t'),
                "br" | "cr" => out.push('\n'),
                _ => {}
            },
            Event::Text(t) if in_text => out.push_str(&t.xml10_content()),
            Event::GeneralRef(g) if in_text => out.extend(reference(&g)),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// A spreadsheet's sheets as rows of `a | b | c`, each under its name.
fn xlsx(bytes: &[u8]) -> anyhow::Result<String> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))?;
    let shared: Vec<String> = match zip_entry(&mut zip, "xl/sharedStrings.xml")? {
        Some(xml) => texts_of(&xml, "si")?,
        None => Vec::new(),
    };
    let names: Vec<String> = match zip_entry(&mut zip, "xl/workbook.xml")? {
        Some(xml) => {
            let mut r = quick_xml::Reader::from_str(&xml);
            let mut v = Vec::new();
            loop {
                match r.read_event()? {
                    Event::Empty(e) | Event::Start(e) if local(e.name().as_ref()) == "sheet" => {
                        if let Some(a) = e.try_get_attribute("name")? {
                            v.push(
                                a.normalized_value(quick_xml::XmlVersion::Implicit1_0)?
                                    .into_owned(),
                            );
                        }
                    }
                    Event::Eof => break,
                    _ => {}
                }
            }
            v
        }
        None => anyhow::bail!("not an Excel workbook"),
    };
    let mut sheets: Vec<(u32, String)> = zip
        .file_names()
        .filter_map(|n| {
            let n = n.ok()?;
            let num = n
                .strip_prefix("xl/worksheets/sheet")?
                .strip_suffix(".xml")?;
            Some((num.parse().ok()?, n.to_string()))
        })
        .collect();
    sheets.sort();
    let mut out = String::new();
    for (i, (_, path)) in sheets.iter().enumerate() {
        let Some(xml) = zip_entry(&mut zip, path)? else {
            continue;
        };
        let fallback = format!("Sheet {}", i + 1);
        out.push_str(&format!("## {}\n", names.get(i).unwrap_or(&fallback)));
        let mut r = quick_xml::Reader::from_str(&xml);
        let (mut row, mut cell, mut kind) = (Vec::<String>::new(), String::new(), String::new());
        let mut in_value = false;
        loop {
            match r.read_event()? {
                Event::Start(e) => match local(e.name().as_ref()) {
                    "c" => {
                        kind = e
                            .try_get_attribute("t")?
                            .map(|a| a.value.into_owned())
                            .unwrap_or_default();
                        cell.clear();
                    }
                    "v" | "t" => in_value = true,
                    _ => {}
                },
                Event::Text(t) if in_value => cell.push_str(&t.xml10_content()),
                Event::GeneralRef(g) if in_value => cell.extend(reference(&g)),
                Event::End(e) => match local(e.name().as_ref()) {
                    "v" | "t" => in_value = false,
                    "c" => {
                        let v = match kind.as_str() {
                            "s" => cell
                                .trim()
                                .parse::<usize>()
                                .ok()
                                .and_then(|i| shared.get(i).cloned())
                                .unwrap_or_default(),
                            "b" => (if cell.trim() == "1" { "TRUE" } else { "FALSE" }).into(),
                            _ => cell.clone(),
                        };
                        row.push(v);
                    }
                    "row" => {
                        while row.last().is_some_and(|c| c.trim().is_empty()) {
                            row.pop();
                        }
                        if !row.is_empty() {
                            out.push_str(&row.join(" | "));
                            out.push('\n');
                        }
                        row.clear();
                    }
                    _ => {}
                },
                Event::Eof => break,
                _ => {}
            }
            if out.len() > MAX_TEXT {
                return Ok(out);
            }
        }
        out.push('\n');
    }
    Ok(out)
}

/// The text inside each `<tag>` element, in order (shared strings).
fn texts_of(xml: &str, tag: &str) -> anyhow::Result<Vec<String>> {
    let mut r = quick_xml::Reader::from_str(xml);
    let (mut out, mut cur, mut inside, mut in_t) = (Vec::new(), String::new(), false, false);
    loop {
        match r.read_event()? {
            Event::Start(e) if local(e.name().as_ref()) == tag => {
                (inside, cur) = (true, String::new())
            }
            Event::Start(e) if inside && local(e.name().as_ref()) == "t" => in_t = true,
            Event::Text(t) if in_t => cur.push_str(&t.xml10_content()),
            Event::GeneralRef(g) if in_t => cur.extend(reference(&g)),
            Event::End(e) if local(e.name().as_ref()) == "t" => in_t = false,
            Event::End(e) if local(e.name().as_ref()) == tag => {
                inside = false;
                out.push(std::mem::take(&mut cur));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// A web page as text, with its title and the links on it.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Page {
    pub title: Option<String>,
    pub text: String,
    pub links: Vec<String>,
}

fn entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" | "#39" => '\'',
        "nbsp" => ' ',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "rsquo" | "lsquo" => '\'',
        "rdquo" | "ldquo" => '"',
        "copy" => '©',
        n => {
            let n = n.strip_prefix('#')?;
            let code = match n.strip_prefix(['x', 'X']) {
                Some(h) => u32::from_str_radix(h, 16).ok()?,
                None => n.parse().ok()?,
            };
            char::from_u32(code)?
        }
    })
}

fn decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[..rest.len().min(12)].find(';');
        match end.and_then(|e| entity(&rest[1..e]).map(|c| (e, c))) {
            Some((e, c)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// An attribute's value in a tag's source (`href="…"`).
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let at = from + i;
        from = at + name.len();
        let before = lower[..at].chars().last();
        if !before.is_some_and(|c| c.is_whitespace()) {
            continue;
        }
        let rest = tag[from..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let v = match rest.chars().next()? {
            q @ ('"' | '\'') => rest[1..].split(q).next()?,
            _ => rest.split(|c: char| c.is_whitespace() || c == '>').next()?,
        };
        return Some(decode(v));
    }
    None
}

/// Elements whose contents aren't the page's text.
const SKIP: &[&str] = &[
    "script", "style", "noscript", "svg", "template", "nav", "footer", "iframe", "form",
];
/// Elements that start a new line.
const BLOCK: &[&str] = &[
    "p",
    "div",
    "br",
    "ul",
    "ol",
    "tr",
    "table",
    "section",
    "article",
    "main",
    "header",
    "blockquote",
    "pre",
    "dd",
    "dt",
    "hr",
    "figcaption",
    "summary",
    "details",
];

/// A web page's title, links, and its text as Markdown: headings, lists,
/// tables, emphasis and code kept, scripts, styles, navigation and footers
/// dropped. The Markdown comes from Zed's `html_to_markdown` (a real HTML
/// parser); the hand-written pass below is the fallback.
pub fn html(src: &str) -> Page {
    let mut page = html_scan(src);
    if let Some(md) = html_to_markdown(src) {
        page.text = cut(md);
    }
    page
}

/// HTML as Markdown, without the page's chrome. `None` if it couldn't be
/// read or holds no text.
pub fn html_to_markdown(src: &str) -> Option<String> {
    use html_to_markdown::markdown::{
        CodeHandler, HeadingHandler, ListHandler, ParagraphHandler, StyledTextHandler, TableHandler,
    };
    use html_to_markdown::{HandleTag, HtmlElement, MarkdownWriter, StartTagOutcome, TagHandler};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Skips what isn't the page's content (as `SKIP`).
    struct Chrome;
    impl HandleTag for Chrome {
        fn should_handle(&self, tag: &str) -> bool {
            tag == "head" || SKIP.contains(&tag)
        }
        fn handle_tag_start(&mut self, _: &HtmlElement, _: &mut MarkdownWriter) -> StartTagOutcome {
            StartTagOutcome::Skip
        }
    }

    let mut handlers: Vec<TagHandler> = vec![
        Rc::new(RefCell::new(Chrome)),
        Rc::new(RefCell::new(ParagraphHandler)),
        Rc::new(RefCell::new(HeadingHandler)),
        Rc::new(RefCell::new(ListHandler)),
        Rc::new(RefCell::new(TableHandler::new())),
        Rc::new(RefCell::new(StyledTextHandler)),
        Rc::new(RefCell::new(CodeHandler)),
    ];
    let md = html_to_markdown::convert_html_to_markdown(src.as_bytes(), &mut handlers).ok()?;
    // At most one blank line in a row.
    let md = md.replace('\u{a0}', " ");
    let mut out = String::with_capacity(md.len());
    let mut blank = 0;
    let mut code = false;
    for line in md.lines() {
        let mut line = line.trim_end();
        if line.trim_start().starts_with("```") {
            code = !code;
        }
        // Indentation from the HTML source; nested lists keep theirs.
        let t = line.trim_start();
        if !code && !t.starts_with(['-', '*', '|']) && !t.starts_with(|c: char| c.is_ascii_digit())
        {
            line = t;
        }
        if !code && line.trim().is_empty() {
            blank += 1;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        out.push_str(line);
    }
    (!out.is_empty()).then_some(out)
}

/// A forgiving HTML-to-text pass: drops scripts, styles, navigation and
/// footers, keeps headings as markdown headings, and lists the links.
fn html_scan(src: &str) -> Page {
    let mut page = Page::default();
    let mut out = String::new();
    let mut rest = src;
    let mut skipping: Option<String> = None;
    let mut in_title = false;
    let mut title = String::new();
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            if skipping.is_none() {
                out.push_str(&decode(rest));
            }
            break;
        };
        let text = &rest[..lt];
        if in_title {
            title.push_str(text);
        } else if skipping.is_none() {
            out.push_str(&decode(text));
        }
        rest = &rest[lt..];
        if let Some(r) = rest.strip_prefix("<!--") {
            rest = r.find("-->").map_or("", |e| &r[e + 3..]);
            continue;
        }
        let Some(gt) = rest.find('>') else { break };
        let tag = &rest[1..gt];
        rest = &rest[gt + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        if let Some(s) = &skipping {
            if closing && *s == name {
                skipping = None;
            }
            continue;
        }
        if !closing && SKIP.contains(&name.as_str()) && !tag.ends_with('/') {
            skipping = Some(name);
            continue;
        }
        match name.as_str() {
            "title" => {
                in_title = !closing;
                if closing && page.title.is_none() {
                    let t = decode(title.trim());
                    page.title =
                        (!t.is_empty()).then(|| t.split_whitespace().collect::<Vec<_>>().join(" "));
                }
            }
            "a" if !closing => {
                if let Some(h) = attr(tag, "href") {
                    page.links.push(h);
                }
            }
            h if h.len() == 2 && h.starts_with('h') && h.as_bytes()[1].is_ascii_digit() => {
                out.push('\n');
                if !closing {
                    let level = (h.as_bytes()[1] - b'0').clamp(1, 6) as usize;
                    out.push_str(&"#".repeat(level));
                    out.push(' ');
                } else {
                    out.push('\n');
                }
            }
            "td" | "th" if closing => out.push_str(" | "),
            "li" if !closing => out.push_str("\n- "),
            b if BLOCK.contains(&b) => out.push('\n'),
            _ => {}
        }
    }
    // Tidy: collapse runs of spaces in each line and of blank lines.
    let mut text = String::with_capacity(out.len());
    let mut blank = 0;
    for line in out.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() || line == "-" {
            blank += 1;
            continue;
        }
        if !text.is_empty() {
            text.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        text.push_str(&line);
    }
    page.text = cut(text);
    page
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn html_becomes_markdown() {
        let md = html_to_markdown(
            "<div><p>Hi <strong>Sam</strong>,</p><p>The <code>deploy</code> failed:</p>\
             <table><tr><th>Step</th><th>Result</th></tr><tr><td>build</td><td>ok</td></tr></table>\
             <ol><li>Retry</li><li>Page me</li></ol></div>",
        )
        .unwrap();
        assert!(md.contains("Hi **Sam**,"), "{md}");
        assert!(md.contains("`deploy`"), "{md}");
        assert!(md.contains("| Step | Result |"), "{md}");
        assert!(md.contains("- Retry\n- Page me"), "{md}");
        assert_eq!(html_to_markdown("<script>x()</script>"), None);
    }

    #[test]
    fn html_keeps_text_headings_and_links() {
        let p = html(
            r#"<html><head><title>Acme &amp; Co</title><style>p{}</style></head>
            <body><nav><a href="/menu">Menu</a></nav>
            <h1>Pricing</h1><p>Plans start at&nbsp;$9.</p>
            <ul><li>Monthly</li><li>Yearly</li></ul>
            <a class="x" href='/refunds?a=1&amp;b=2'>Refunds</a>
            <script>var x = "<p>no</p>";</script><!-- <p>hidden</p> -->
            <footer>© Acme</footer></body></html>"#,
        );
        assert_eq!(p.title.as_deref(), Some("Acme & Co"));
        assert!(p.text.contains("# Pricing"));
        assert!(p.text.contains("Plans start at $9."));
        assert!(p.text.contains("- Monthly\n- Yearly"));
        for gone in ["Menu", "no", "hidden", "© Acme", "p{}"] {
            assert!(!p.text.contains(gone), "{gone} in {:?}", p.text);
        }
        assert_eq!(p.links, vec!["/refunds?a=1&b=2"]);
    }

    fn zipped(files: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        for (name, body) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
        buf.into_inner()
    }

    #[test]
    fn reads_word_and_excel() {
        let docx_bytes = zipped(&[(
            "word/document.xml",
            r#"<w:document xmlns:w="x"><w:body>
                <w:p><w:r><w:t>Refund policy</w:t></w:r></w:p>
                <w:p><w:r><w:t xml:space="preserve">Within 30 days &amp; </w:t></w:r><w:r><w:t>no questions.</w:t></w:r></w:p>
            </w:body></w:document>"#,
        )]);
        let t = file_text("policy.docx", &docx_bytes).unwrap();
        assert!(t.contains("Refund policy\n"), "{t:?}");
        assert!(t.contains("Within 30 days & no questions."), "{t:?}");

        let xlsx_bytes = zipped(&[
            (
                "xl/workbook.xml",
                r#"<workbook><sheets><sheet name="Prices" sheetId="1"/></sheets></workbook>"#,
            ),
            (
                "xl/sharedStrings.xml",
                r#"<sst><si><t>Plan</t></si><si><t>Pro</t></si></sst>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData>
                    <row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="inlineStr"><is><t>Price</t></is></c></row>
                    <row r="2"><c r="A2" t="s"><v>1</v></c><c r="B2"><v>29</v></c></row>
                </sheetData></worksheet>"#,
            ),
        ]);
        let t = file_text("prices.xlsx", &xlsx_bytes).unwrap();
        assert_eq!(t.trim(), "## Prices\nPlan | Price\nPro | 29");

        assert!(file_text("notes.md", b"# Hi").unwrap() == "# Hi");
        assert!(file_text("x.exe", b"MZ").is_err());
        assert!(file_text("fake.pdf", b"hello").is_err());
        assert!(supported("A.PDF") && !supported("a.png"));
    }
}
