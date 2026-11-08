//! Just enough XML for UPnP: the content of elements by name (ignoring
//! namespace prefixes) and escaping. Device descriptions and SOAP answers
//! are small and regular; nothing here needs a full parser.

/// The inner content of every `<tag …>…</tag>` (also `<x:tag>`), outermost
/// first; nested elements of the same name are skipped over. One pass over
/// the tags, so any input takes linear time.
pub fn elements<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    // Open elements of this name, and where the outermost one's content
    // starts.
    let mut depth = 0usize;
    let mut content = 0;
    let mut i = 0;
    while let Some(off) = xml.get(i..).and_then(|r| r.find('<')) {
        let start = i + off + 1;
        let rest = &xml[start..];
        if let Some(close) = rest.strip_prefix('/') {
            // A closing tag; without its '>' nothing more closes.
            let Some(gt) = close.find('>') else { break };
            if close[..gt].trim().rsplit(':').next() == Some(tag) && depth > 0 {
                depth -= 1;
                if depth == 0 {
                    out.push(&xml[content..start - 1]);
                }
            }
            i = start + 1 + gt + 1;
            continue;
        }
        if rest.starts_with('?') || rest.starts_with('!') {
            i = start;
            continue;
        }
        if name_is(rest, tag) {
            // Without its '>' the element cannot open.
            let Some(gt) = rest.find('>') else { break };
            let empty = rest[..gt].ends_with('/');
            if depth == 0 && empty {
                out.push("");
            } else if !empty {
                if depth == 0 {
                    content = start + gt + 1;
                }
                depth += 1;
            }
            i = start + gt + 1;
            continue;
        }
        i = start;
    }
    out
}

/// The unescaped text of the first `tag` element.
pub fn text(xml: &str, tag: &str) -> Option<String> {
    elements(xml, tag).first().map(|s| unescape(s.trim()))
}

/// Whether the tag name at the start of `rest` (up to `>`, `/` or white
/// space) is `tag`, also with a prefix. Names are short: only the first
/// 256 characters are looked at, so a run of `<` costs linear time.
fn name_is(rest: &str, tag: &str) -> bool {
    let mut end = rest.len();
    for (n, (i, c)) in rest.char_indices().enumerate() {
        if c == '>' || c == '/' || c.is_whitespace() {
            end = i;
            break;
        }
        if n == 256 {
            return false;
        }
    }
    let name = &rest[..end];
    name.rsplit(':').next().unwrap_or(name) == tag
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
        // Only the next few bytes: searching all the rest for each '&'
        // made a text of many of them quadratic.
        let near = &after.as_bytes()[..after.len().min(11)];
        let Some(semi) = near.iter().position(|&b| b == b';') else {
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
        // Unclosed, stray closes, comments and empty ones.
        assert!(elements("<b>open", "b").is_empty());
        assert_eq!(elements("</b><b>x</b></b><b>y</b>", "b"), vec!["x", "y"]);
        assert_eq!(elements("<?xml?><b/><x:b >z</x:b >", "b"), vec!["", "z"]);
        assert_eq!(elements("<bb>no</bb><b\t>yes</b>", "b"), vec!["yes"]);
    }

    #[test]
    fn crafted_documents_take_linear_time() {
        let t = std::time::Instant::now();
        for x in [
            "<".repeat(200_000),
            "</".repeat(200_000) + ">",
            "<b>".repeat(100_000) + &"</b>".repeat(100_000),
            "<b ".repeat(100_000) + ">",
            "&".repeat(200_000) + ";",
        ] {
            let _ = elements(&x, "b");
            let _ = text(&x, "b");
            let _ = unescape(&x);
        }
        let nested = "<b>".repeat(1000) + "x" + &"</b>".repeat(1000);
        assert_eq!(elements(&nested, "b").len(), 1);
        assert!(t.elapsed().as_secs() < 5, "{:?}", t.elapsed());
    }
}
