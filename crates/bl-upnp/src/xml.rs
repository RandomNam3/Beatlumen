//! Just enough XML for UPnP: the content of elements by name (ignoring
//! namespace prefixes) and escaping. Device descriptions and SOAP answers
//! are small and regular; nothing here needs a full parser.

/// The inner content of every `<tag …>…</tag>` (also `<x:tag>`), outermost
/// first; nested elements of the same name are skipped over.
pub fn elements<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some((content_start, empty)) = find_open(xml, tag, pos) {
        if empty {
            out.push("");
            pos = content_start;
            continue;
        }
        // Matching close, counting nested opens of the same name.
        let mut depth = 1;
        let mut p = content_start;
        let mut end = None;
        while depth > 0 {
            let next_open = find_open(xml, tag, p);
            let next_close = find_close(xml, tag, p);
            match (next_open, next_close) {
                (Some((o, e)), Some((c, _))) if o < c => {
                    if !e {
                        depth += 1;
                    }
                    p = o;
                }
                (_, Some((c, ca))) => {
                    depth -= 1;
                    p = ca;
                    if depth == 0 {
                        end = Some(c);
                    }
                }
                _ => break,
            }
        }
        let Some(end) = end else { break };
        out.push(&xml[content_start..end]);
        pos = p;
    }
    out
}

/// The unescaped text of the first `tag` element.
pub fn text(xml: &str, tag: &str) -> Option<String> {
    elements(xml, tag).first().map(|s| unescape(s.trim()))
}

/// Finds `<tag` or `<prefix:tag` followed by `>`, ` ` or `/` at or after
/// `from`; returns where its content starts and whether it is empty
/// (`<tag/>`).
fn find_open(xml: &str, tag: &str, from: usize) -> Option<(usize, bool)> {
    let mut i = from;
    while let Some(off) = xml.get(i..)?.find('<') {
        let start = i + off + 1;
        let rest = &xml[start..];
        if rest.starts_with('/') || rest.starts_with('?') || rest.starts_with('!') {
            i = start;
            continue;
        }
        let name_end = rest
            .find(|c: char| c == '>' || c == '/' || c.is_whitespace())
            .unwrap_or(rest.len());
        let name = &rest[..name_end];
        let local = name.rsplit(':').next().unwrap_or(name);
        if local == tag {
            let gt = rest.find('>')?;
            return Some((start + gt + 1, rest[..gt].ends_with('/')));
        }
        i = start;
    }
    None
}

/// Finds `</tag>` or `</prefix:tag>`; returns (its start, after it).
fn find_close(xml: &str, tag: &str, from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while let Some(off) = xml.get(i..)?.find("</") {
        let start = i + off;
        let rest = &xml[start + 2..];
        let gt = rest.find('>')?;
        let name = rest[..gt].trim();
        if name.rsplit(':').next() == Some(tag) {
            return Some((start, start + 2 + gt + 1));
        }
        i = start + 2;
    }
    None
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        let Some(semi) = after.find(';').filter(|&j| j <= 10) else {
            out.push('&');
            rest = &after[1..];
            continue;
        };
        let entity = &after[1..semi];
        let c = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e if e.starts_with("#x") => u32::from_str_radix(&e[2..], 16)
                .ok()
                .and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_and_prefixed() {
        let x = "<a><s:b x='1'>one<b>two</b></s:b><b/><b>three</b></a>";
        assert_eq!(elements(x, "b"), vec!["one<b>two</b>", "", "three"]);
        assert_eq!(text(x, "b").as_deref(), Some("one<b>two</b>"));
        assert_eq!(
            unescape("a &amp; b &lt;c&gt; &#228;&#x20AC;"),
            "a & b <c> ä€"
        );
        assert_eq!(escape("<a & 'b'>"), "&lt;a &amp; &apos;b&apos;&gt;");
    }
}
