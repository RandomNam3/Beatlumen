//! TTML song texts (timed text with word times, as Apple Music and others
//! write them) as enhanced LRC, the form everything here works with: `[v:N]`
//! for voices, parentheses for backing voices, `<mm:ss.xx>` for words.
//! Translations and transliterations in the file are left out.

/// Whether a text is TTML and not LRC or plain words.
pub fn is_ttml(text: &str) -> bool {
    let t = text.trim_start_matches(['\u{feff}', ' ', '\t', '\r', '\n']);
    (t.starts_with("<?xml") || t.starts_with("<tt") || t.starts_with("<!--")) && t.contains("<tt")
}

struct Word {
    start: Option<i64>,
    end: Option<i64>,
    text: String,
    backing: bool,
    /// Its span is still open: more text belongs to it.
    open: bool,
}

#[derive(Default)]
struct Line {
    start: Option<i64>,
    agent: String,
    words: Vec<Word>,
}

/// A tag's name without its prefix, in lower case.
fn local_name(tag: &str) -> String {
    let name = tag
        .split(|c: char| c.is_whitespace() || c == '/')
        .next()
        .unwrap_or("");
    name.rsplit(':').next().unwrap_or("").to_ascii_lowercase()
}

/// The value of an attribute, also with a prefix (`ttm:agent`).
fn attribute(tag: &str, wanted: &str) -> Option<String> {
    let mut rest = tag.split_once(|c: char| c.is_whitespace()).map(|x| x.1)?;
    loop {
        let eq = rest.find('=')?;
        let name = rest[..eq].trim();
        let after = rest[eq + 1..].trim_start();
        let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let body = &after[1..];
        let close = body.find(quote)?;
        let local = name.rsplit(':').next().unwrap_or("");
        if local.eq_ignore_ascii_case(wanted) {
            return Some(unescape(&body[..close]));
        }
        rest = &body[close + 1..];
    }
}

fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        // Only the next few bytes: a search through all the rest for each
        // '&' made a text of many of them quadratic.
        let near = &rest.as_bytes()[..rest.len().min(11)];
        let Some(end) = near.iter().position(|&b| b == b';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let value = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match value {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
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

/// `83.5s`, `1500ms`, `1.5m`, `1:23.456`, `01:23:45.678`, `00:01:02:15` (with
/// frames) or a plain number of seconds, in milliseconds.
fn parse_time(text: &str) -> Option<i64> {
    let t = text.trim();
    let seconds = |v: f64| Some((v * 1000.0).round() as i64).filter(|ms| *ms >= 0);
    if let Some(v) = t.strip_suffix("ms") {
        return v.parse::<f64>().ok().and_then(|v| seconds(v / 1000.0));
    }
    if let Some(v) = t.strip_suffix('s') {
        return v.parse::<f64>().ok().and_then(seconds);
    }
    if let Some(v) = t.strip_suffix('m') {
        return v.parse::<f64>().ok().and_then(|v| seconds(v * 60.0));
    }
    if let Some(v) = t.strip_suffix('h') {
        return v.parse::<f64>().ok().and_then(|v| seconds(v * 3600.0));
    }
    let parts: Vec<&str> = t.split(':').collect();
    let number = |p: &str| p.parse::<f64>().ok().filter(|v| *v >= 0.0);
    match parts.as_slice() {
        [s] => seconds(number(s)?),
        [m, s] => seconds(number(m)? * 60.0 + number(s)?),
        [h, m, s] => seconds(number(h)? * 3600.0 + number(m)? * 60.0 + number(s)?),
        [h, m, s, f] => {
            seconds(number(h)? * 3600.0 + number(m)? * 60.0 + number(s)? + number(f)? / 30.0)
        }
        _ => None,
    }
}

fn clock(ms: i64) -> String {
    let cs = ms.max(0) / 10;
    format!("{:02}:{:02}.{:02}", cs / 6000, cs / 100 % 60, cs % 100)
}

/// Collapses runs of white space, as XML does for text.
fn collapse(text: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = true;
        } else {
            if space {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    if space {
        out.push(' ');
    }
    out
}

/// A span's role is left out when it is not sung as written.
fn skipped_role(role: &str) -> bool {
    matches!(role, "x-translation" | "x-roman" | "x-pronunciation")
}

fn read_lines(text: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut line: Option<Line> = None;
    let mut rest = text;
    // Open spans: (a word, backing voice, left out).
    let mut spans: Vec<(bool, bool, bool)> = Vec::new();
    let mut div_agent = String::new();
    while !rest.is_empty() {
        let Some(open) = rest.find('<') else { break };
        // Each open span's flags include those of the spans around it, so
        // the innermost one tells (no walk through all open spans).
        let (in_backing, in_skipped) = spans.last().map_or((false, false), |s| (s.1, s.2));
        if let Some(l) = line.as_mut().filter(|_| !in_skipped) {
            let t = collapse(&unescape(&rest[..open]));
            let blank = t.trim().is_empty();
            let backing = in_backing;
            match l.words.last_mut() {
                Some(w) if w.open => w.text.push_str(&t),
                // Space between spans belongs to the word before it.
                Some(w) if blank && !t.is_empty() && !w.text.ends_with(' ') => w.text.push(' '),
                // Text that is no span's.
                Some(w) if !blank && w.start.is_none() => w.text.push_str(&t),
                _ if !blank => l.words.push(Word {
                    start: None,
                    end: None,
                    text: t,
                    backing,
                    open: false,
                }),
                _ => {}
            }
        }
        let after = &rest[open + 1..];
        if let Some(r) = after.strip_prefix("!--") {
            rest = r.find("-->").map_or("", |e| &r[e + 3..]);
            continue;
        }
        let Some(close) = after.find('>') else { break };
        let tag = after[..close].trim();
        rest = &after[close + 1..];
        if tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        let closing = tag.starts_with('/');
        let self_closing = tag.ends_with('/');
        match local_name(tag.trim_start_matches('/')).as_str() {
            "div" if !closing => div_agent = attribute(tag, "agent").unwrap_or_default(),
            "p" if !closing => {
                spans.clear();
                let l = Line {
                    start: attribute(tag, "begin").and_then(|b| parse_time(&b)),
                    agent: attribute(tag, "agent").unwrap_or_else(|| div_agent.clone()),
                    words: Vec::new(),
                };
                if !self_closing {
                    line = Some(l);
                }
            }
            "p" => lines.extend(line.take()),
            "br" => {
                if let Some(w) = line.as_mut().and_then(|l| l.words.last_mut()) {
                    if !w.text.ends_with(' ') {
                        w.text.push(' ');
                    }
                }
            }
            "span" if closing => {
                if spans.pop().is_some_and(|s| s.0) {
                    if let Some(w) = line.as_mut().and_then(|l| l.words.last_mut()) {
                        w.open = false;
                    }
                }
            }
            "span" if !self_closing => {
                let Some(l) = line.as_mut() else {
                    continue;
                };
                let role = attribute(tag, "role").unwrap_or_default();
                let skipped = skipped_role(&role) || in_skipped;
                let backing = role == "x-bg" || in_backing;
                let start = attribute(tag, "begin").and_then(|b| parse_time(&b));
                // A span that holds other spans (backing voices) is no word.
                let word = start.is_some() && !skipped && !rest.trim_start().starts_with('<');
                if word {
                    l.words.push(Word {
                        start,
                        end: attribute(tag, "end").and_then(|e| parse_time(&e)),
                        text: String::new(),
                        backing,
                        open: true,
                    });
                }
                spans.push((word, backing, skipped));
            }
            _ => {}
        }
    }
    lines
}

/// The text as enhanced LRC; `None` when it holds no song text.
pub fn ttml_to_lrc(text: &str) -> Option<String> {
    let body = text.find("<body").map_or(text, |i| &text[i..]);
    let lines = read_lines(body);
    // A number for each voice, in the order they first sing; one alone is no duet.
    let mut agents: std::collections::HashMap<&str, usize> = Default::default();
    for l in &lines {
        if !l.agent.is_empty() {
            let n = agents.len() + 1;
            agents.entry(&l.agent).or_insert(n);
        }
    }
    let mut out = String::new();
    let mut voice = 0usize;
    let mut any = false;
    for l in &lines {
        if l.words.iter().all(|w| w.text.trim().is_empty()) {
            continue;
        }
        if agents.len() > 1 {
            let v = agents.get(l.agent.as_str()).copied().unwrap_or(0);
            if v != voice && v > 0 {
                out += &format!("[v:{v}]\n");
            }
            voice = v;
        }
        let start = l.start.or_else(|| l.words.iter().find_map(|w| w.start));
        if let Some(t) = start {
            out += &format!("[{}]", clock(t));
        }
        let words: Vec<&Word> = l.words.iter().filter(|w| !w.text.is_empty()).collect();
        // The line's ends without their spaces.
        let last = words.len().saturating_sub(1);
        let timed = start.is_some() && words.iter().any(|w| w.start.is_some());
        let mut in_backing = false;
        for (i, w) in words.iter().enumerate() {
            let next = words.get(i + 1);
            let next_start = next.and_then(|x| x.start);
            let mut word = if i == last {
                w.text.trim_end().to_string()
            } else {
                w.text.clone()
            };
            if i == 0 {
                word = word.trim_start().to_string();
            }
            // Backing voices are what stands in parentheses.
            if w.backing && !in_backing && !word.trim_start().starts_with('(') {
                word.insert(0, '(');
            }
            let following_backing = next.is_some_and(|x| x.backing);
            if w.backing && !following_backing && !word.contains(')') {
                let trimmed = word.trim_end().len();
                word.insert(trimmed, ')');
            }
            in_backing = w.backing && following_backing;
            if timed {
                if let Some(t) = w.start {
                    out += &format!("<{}>", clock(t));
                }
            }
            // A pause before the next word: this one ends earlier. Its end
            // stands before the space, or it would read as the next one's start.
            let body = word.trim_end();
            out += body;
            if let Some(e) = w.end.filter(|_| timed) {
                if next_start.is_none_or(|n| e < n) {
                    out += &format!("<{}>", clock(e));
                }
            }
            out += &word[body.len()..];
        }
        out.push('\n');
        any = true;
    }
    any.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_times_voices_and_backing_voices() {
        let ttml = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata" xml:lang="en">
  <head><metadata><ttm:agent type="person" xml:id="v1"/><ttm:agent type="person" xml:id="v2"/></metadata></head>
  <body dur="3:00.000">
    <div begin="1.000" end="9.000">
      <p begin="1.000" end="3.000" ttm:agent="v1"><span begin="1.000" end="1.500">Hel</span><span begin="1.500" end="2.000">lo</span> <span begin="2.100" end="2.900">world &amp; you</span></p>
      <p begin="4.000" end="6.000" ttm:agent="v2"><span begin="4.000" end="5.000">Second</span>
        <span begin="5.000" end="6.000">line</span>
        <span ttm:role="x-bg"><span begin="6.100" end="6.500">oh</span> <span begin="6.500" end="7.000">yeah</span></span>
        <span ttm:role="x-translation" xml:lang="de">Zweite Zeile</span>
      </p>
    </div>
  </body>
</tt>"#;
        assert!(is_ttml(ttml));
        let lrc = ttml_to_lrc(ttml).unwrap();
        let lines: Vec<&str> = lrc.lines().collect();
        assert_eq!(lines[0], "[v:1]");
        assert_eq!(
            lines[1],
            "[00:01.00]<00:01.00>Hel<00:01.50>lo<00:02.00> <00:02.10>world & you<00:02.90>"
        );
        assert_eq!(lines[2], "[v:2]");
        assert!(lines[3].starts_with("[00:04.00]<00:04.00>Second <00:05.00>line"));
        assert!(
            lines[3].contains("<00:06.10>(oh <00:06.50>yeah)"),
            "{}",
            lines[3]
        );
        assert!(!lrc.contains("Zeile"), "translations stay out");
    }

    #[test]
    fn lines_without_word_times_and_one_voice() {
        let ttml = r#"<tt><body><div>
            <p begin="00:01:02.500" end="00:01:05.000" ttm:agent="v1">Only a line</p>
            <p begin="66s">Another &#x263A; line</p>
        </div></body></tt>"#;
        let lrc = ttml_to_lrc(ttml).unwrap();
        assert_eq!(lrc, "[01:02.50]Only a line\n[01:06.00]Another ☺ line\n");
    }

    #[test]
    fn plain_text_without_any_times() {
        let lrc = ttml_to_lrc("<tt><body><div><p>One</p><p>Two</p></div></body></tt>").unwrap();
        assert_eq!(lrc, "One\nTwo\n");
    }

    #[test]
    fn times_in_every_form() {
        for (t, ms) in [
            ("1.5", 1500),
            ("1.5s", 1500),
            ("1500ms", 1500),
            ("1:02.5", 62500),
            ("01:02:03.250", 3723250),
            ("0.5m", 30000),
            ("00:00:01:15", 1500),
        ] {
            assert_eq!(parse_time(t), Some(ms), "{t}");
        }
        assert_eq!(parse_time("soon"), None);
        assert_eq!(parse_time("-1s"), None);
    }

    #[test]
    fn what_is_not_ttml_is_left_alone() {
        assert!(!is_ttml("[00:01.00]line\n"));
        assert!(!is_ttml("<00:01.00>word"));
        assert!(!is_ttml("Just words <tt> in a text"));
        assert_eq!(ttml_to_lrc("<tt><body></body></tt>"), None);
        assert_eq!(
            ttml_to_lrc("<tt><body><div><p></p></div></body></tt>"),
            None
        );
        // Broken input does no harm.
        assert_eq!(ttml_to_lrc("<tt><body><div><p begin="), None);
    }

    #[test]
    fn crafted_texts_take_linear_time() {
        let t = std::time::Instant::now();
        let p = |body: &str| format!("<tt><body><div><p begin=\"1s\">{body}</p></div></body></tt>");
        // Many '&' without ';', deeply nested spans, many words in a line,
        // many voices.
        let _ = ttml_to_lrc(&p(&"&".repeat(200_000)));
        let nested = "<span>".repeat(50_000) + &"x<b/>".repeat(50_000);
        let _ = ttml_to_lrc(&p(&nested));
        let words = "<span begin=\"1s\" end=\"2s\">w</span> ".repeat(30_000);
        assert!(ttml_to_lrc(&p(&words)).is_some());
        let voices: String = (0..20_000)
            .map(|i| format!("<p begin=\"1s\" ttm:agent=\"v{i}\">x</p>"))
            .collect();
        let lrc = ttml_to_lrc(&format!("<tt><body><div>{voices}</div></body></tt>")).unwrap();
        assert!(lrc.contains("[v:20000]"));
        assert!(t.elapsed().as_secs() < 10, "{:?}", t.elapsed());
    }
}
