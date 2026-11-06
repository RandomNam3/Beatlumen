//! Asking about one artist, step by step: the caller makes each request and
//! hands the answer back.

use crate::{Artist, Link, Related};
use bl_json::Value;

/// Who is asked; each has its own pace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    MusicBrainz,
    Wikimedia,
    ListenBrainz,
}

/// When each service may be asked next.
#[derive(Debug, Clone, Default)]
pub struct Pacer {
    next: [f64; 3],
}

impl Service {
    /// Seconds between requests. MusicBrainz allows one per second and bans
    /// those who do not keep to it.
    fn every(self) -> f64 {
        match self {
            Service::MusicBrainz => 1.1,
            Service::Wikimedia => 0.2,
            Service::ListenBrainz => 1.0,
        }
    }
}

impl Pacer {
    pub fn ready(&self, s: Service, now: f64) -> bool {
        now >= self.next[s as usize]
    }

    pub fn mark(&mut self, s: Service, now: f64) {
        self.next[s as usize] = now + s.every();
    }

    /// A service said it wants a pause.
    pub fn hold(&mut self, s: Service, now: f64, seconds: f64) {
        self.next[s as usize] = now + seconds.max(s.every());
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub service: Service,
    pub url: String,
}

/// Nothing is remembered from this: it is worth asking again later.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Failure {
    pub service: Service,
    /// How long the service asked to be left alone (seconds).
    pub pause: f64,
}

/// How much is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// For recommendations: MusicBrainz alone.
    Genres,
    /// For an artist page: also the biography, the picture and who is similar.
    Page,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Search,
    Details,
    Sitelinks,
    ImageClaim,
    Summary,
    ImageInfo,
    Similar,
    Done,
}

const MUSICBRAINZ: &str = "https://musicbrainz.org/ws/2";
const WIKIDATA: &str = "https://www.wikidata.org/w/api.php";
const COMMONS: &str = "https://commons.wikimedia.org/w/api.php";
// A model of ListenBrainz's own, the one its site uses for similar artists.
const SIMILAR: &str = "https://labs.api.listenbrainz.org/similar-artists/json";
const SIMILAR_MODEL: &str =
    "session_based_days_7500_session_300_contribution_5_threshold_10_limit_100_filter_True_skip_30";

const MOST_SIMILAR: usize = 12;
const MOST_GENRES: usize = 8;

/// Asks about one artist.
#[derive(Debug, Clone)]
pub struct Lookup {
    name: String,
    language: String,
    depth: Depth,
    step: Step,
    artist: Artist,
    wikidata: String,
    /// Language and title of the Wikipedia page.
    wiki: Option<(String, String)>,
    image_file: String,
    found: bool,
}

impl Lookup {
    /// `mbid`: the artist's id when the tags name it (nothing to search then).
    pub fn new(name: &str, mbid: Option<&str>, language: &str, depth: Depth) -> Lookup {
        let id = mbid.filter(|m| is_mbid(m));
        Lookup {
            name: name.trim().to_string(),
            language: language.to_string(),
            depth,
            step: if id.is_some() {
                Step::Details
            } else {
                Step::Search
            },
            artist: Artist {
                mbid: id.unwrap_or_default().to_string(),
                ..Artist::default()
            },
            wikidata: String::new(),
            wiki: None,
            image_file: String::new(),
            found: false,
        }
    }

    /// What to ask next; `None` when it is done.
    pub fn request(&self) -> Option<Request> {
        let mb = |url: String| {
            Some(Request {
                service: Service::MusicBrainz,
                url,
            })
        };
        let wiki = |url: String| {
            Some(Request {
                service: Service::Wikimedia,
                url,
            })
        };
        match self.step {
            Step::Search => mb(format!(
                "{MUSICBRAINZ}/artist?query={}&fmt=json&limit=5",
                encode(&format!("artist:\"{}\"", query_escape(&self.name)))
            )),
            Step::Details => mb(format!(
                "{MUSICBRAINZ}/artist/{}?inc=aliases+genres+artist-rels+url-rels&fmt=json",
                self.artist.mbid
            )),
            Step::Sitelinks => wiki(format!(
                "{WIKIDATA}?action=wbgetentities&ids={}&props=sitelinks&sitefilter={}&format=json&origin=*",
                self.wikidata,
                encode(&format!("{}wiki|enwiki", self.language))
            )),
            Step::ImageClaim => wiki(format!(
                "{WIKIDATA}?action=wbgetclaims&entity={}&property=P18&format=json&origin=*",
                self.wikidata
            )),
            Step::Summary => {
                let (lang, title) = self.wiki.as_ref()?;
                wiki(format!(
                    "https://{lang}.wikipedia.org/api/rest_v1/page/summary/{}",
                    encode(&title.replace(' ', "_"))
                ))
            }
            Step::ImageInfo => wiki(format!(
                "{COMMONS}?action=query&titles={}&prop=imageinfo&iiprop=url%7Cextmetadata&iiurlwidth=640&iiextmetadatafilter=Artist%7CLicenseShortName&format=json&origin=*",
                encode(&format!("File:{}", self.image_file))
            )),
            Step::Similar => Some(Request {
                service: Service::ListenBrainz,
                url: format!(
                    "{SIMILAR}?artist_mbids={}&algorithm={SIMILAR_MODEL}",
                    self.artist.mbid
                ),
            }),
            Step::Done => None,
        }
    }

    /// Takes the answer to [`Lookup::request`]; a status of 0 is no answer. A
    /// service that is down or wants a pause is a [`Failure`]: the lookup can
    /// ask the same again after a while. Only the icing (biography, picture,
    /// similar artists) simply goes on without.
    pub fn answer(&mut self, status: u16, body: &[u8]) -> Result<(), Failure> {
        let service = match self.request() {
            Some(r) => r.service,
            None => return Ok(()),
        };
        let essential = matches!(self.step, Step::Search | Step::Details);
        if status == 429 || status == 0 || status >= 500 {
            if essential {
                // MusicBrainz answers 503 now and then when it is busy, to
                // anybody; the same question works moments later. The step
                // stays, so asking again is all it takes.
                return Err(Failure {
                    service,
                    pause: if status == 429 { 30.0 } else { 3.0 },
                });
            }
            self.advance();
            return Ok(());
        }
        let value = if (200..300).contains(&status) {
            bl_json::parse_bytes(body).ok()
        } else {
            None
        };
        match self.step {
            Step::Search => match value.as_ref().and_then(|v| confident(v, &self.name)) {
                Some((id, ambiguous)) => {
                    self.artist.mbid = id;
                    self.artist.ambiguous = ambiguous;
                    self.step = Step::Details;
                }
                None => self.step = Step::Done,
            },
            Step::Details => {
                let Some(v) = value else {
                    // An id in the tags that does not exist (any more).
                    self.step = Step::Done;
                    return Ok(());
                };
                let ambiguous = self.artist.ambiguous;
                self.artist = read_details(&v);
                self.artist.ambiguous = ambiguous;
                self.found = !self.artist.mbid.is_empty();
                self.wikidata = wikidata_id(&v).unwrap_or_default();
                self.wiki = wikipedia_page(&v);
                self.artist.complete = self.depth == Depth::Page;
                self.advance();
            }
            Step::Sitelinks => {
                if let Some(v) = &value {
                    if let Some(page) = sitelink(v, &self.wikidata, &self.language) {
                        self.wiki = Some(page);
                    }
                }
                self.advance();
            }
            Step::ImageClaim => {
                if let Some(v) = &value {
                    self.image_file = v
                        .get("claims")
                        .get("P18")
                        .at(0)
                        .get("mainsnak")
                        .get("datavalue")
                        .get("value")
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                }
                self.advance();
            }
            Step::Summary => {
                if let Some(v) = &value {
                    // A page that lists several meanings tells nothing.
                    if v.str_or("type", "") != "disambiguation" {
                        self.artist.biography = v.str_or("extract", "").trim().to_string();
                        self.artist.biography_url = v
                            .get("content_urls")
                            .get("desktop")
                            .str_or("page", "")
                            .to_string();
                    }
                }
                self.advance();
            }
            Step::ImageInfo => {
                if let Some(v) = &value {
                    read_image(v, &mut self.artist);
                }
                self.advance();
            }
            Step::Similar => {
                if let Some(v) = &value {
                    self.artist.similar = read_similar(v);
                }
                self.advance();
            }
            Step::Done => {}
        }
        Ok(())
    }

    fn advance(&mut self) {
        let page = self.depth == Depth::Page;
        let next = |step: Step| -> Step {
            match step {
                Step::Details => {
                    if !page {
                        Step::Done
                    } else if !self.wikidata.is_empty() {
                        Step::Sitelinks
                    } else if self.wiki.is_some() {
                        Step::Summary
                    } else {
                        Step::Similar
                    }
                }
                Step::Sitelinks => Step::ImageClaim,
                Step::ImageClaim => {
                    if self.wiki.is_some() {
                        Step::Summary
                    } else if !self.image_file.is_empty() {
                        Step::ImageInfo
                    } else {
                        Step::Similar
                    }
                }
                Step::Summary => {
                    if !self.image_file.is_empty() {
                        Step::ImageInfo
                    } else {
                        Step::Similar
                    }
                }
                Step::ImageInfo => Step::Similar,
                _ => Step::Done,
            }
        };
        self.step = next(self.step);
    }

    /// What came of it (`None`: nothing to be sure of).
    pub fn finish(self) -> Option<Artist> {
        self.found.then_some(self.artist)
    }
}

/// Letters and digits only, lower case: how two names are told the same.
fn plain(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_mbid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

/// The artist a search found with confidence: the best row, when MusicBrainz
/// scores it 90 or more and its name or one of its aliases is the one asked
/// for. A second row of that name makes it ambiguous. Anything less is nobody.
fn confident(found: &Value, name: &str) -> Option<(String, bool)> {
    let wanted = plain(name);
    if wanted.is_empty() {
        return None;
    }
    let rows = found.get("artists").as_array()?;
    let same = |row: &Value| -> bool {
        let score = row.get("score").as_u64().unwrap_or(0);
        score >= 90
            && (plain(row.str_or("name", "")) == wanted
                || row.get("aliases").as_array().is_some_and(|aliases| {
                    aliases
                        .iter()
                        .any(|a| plain(a.str_or("name", "")) == wanted)
                }))
    };
    let first = rows.first().filter(|r| same(r))?;
    let id = first.str_or("id", "");
    if !is_mbid(id) {
        return None;
    }
    let others = rows.iter().skip(1).any(same);
    Some((id.to_string(), others))
}

fn read_details(v: &Value) -> Artist {
    let mut a = Artist {
        mbid: v.str_or("id", "").to_string(),
        name: v.str_or("name", "").to_string(),
        kind: v.str_or("type", "").to_lowercase(),
        country: v.str_or("country", "").to_string(),
        area: v.get("area").str_or("name", "").to_string(),
        begin_area: v.get("begin-area").str_or("name", "").to_string(),
        begin: v.get("life-span").str_or("begin", "").to_string(),
        end: v.get("life-span").str_or("end", "").to_string(),
        ended: v.get("life-span").bool_or("ended", false),
        ..Artist::default()
    };
    let mut genres: Vec<(&str, u64)> = v
        .get("genres")
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|g| {
            Some((
                g.get("name").as_str()?,
                g.get("count").as_u64().unwrap_or(0),
            ))
        })
        .collect();
    genres.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(y.0)));
    a.genres = genres
        .into_iter()
        .take(MOST_GENRES)
        .map(|(g, _)| g.to_string())
        .collect();
    for r in v.get("relations").as_array().into_iter().flatten() {
        let kind = r.str_or("type", "");
        if kind == "member of band" {
            let related = Related {
                name: r.get("artist").str_or("name", "").to_string(),
                mbid: r.get("artist").str_or("id", "").to_string(),
                begin: r.str_or("begin", "").to_string(),
                end: r.str_or("end", "").to_string(),
                ended: r.bool_or("ended", false),
            };
            if related.name.is_empty() {
                continue;
            }
            // Backward: they are a member of this one. Several entries for one
            // person (instruments, periods) stay one.
            let list = if r.str_or("direction", "") == "backward" {
                &mut a.members
            } else {
                &mut a.groups
            };
            if !list.iter().any(|m| m.mbid == related.mbid) {
                list.push(related);
            }
        } else if r.str_or("target-type", "") == "url" && !r.bool_or("ended", false) {
            let url = r.get("url").str_or("resource", "");
            let link = match kind {
                "official homepage" => Some("homepage"),
                "bandcamp" => Some("bandcamp"),
                "soundcloud" => Some("soundcloud"),
                "youtube" => Some("youtube"),
                "discogs" => Some("discogs"),
                _ => None,
            };
            if let (Some(k), true) = (link, url.starts_with("http")) {
                if !a.links.iter().any(|l| l.kind == k) {
                    a.links.push(Link {
                        kind: k.to_string(),
                        url: url.to_string(),
                    });
                }
            }
        }
    }
    // Who is in it now first.
    a.members.sort_by_key(|m| m.ended);
    a
}

fn url_relation<'a>(v: &'a Value, kind: &str) -> Option<&'a str> {
    v.get("relations")
        .as_array()?
        .iter()
        .find(|r| r.str_or("type", "") == kind)
        .map(|r| r.get("url").str_or("resource", ""))
}

fn wikidata_id(v: &Value) -> Option<String> {
    let url = url_relation(v, "wikidata")?;
    let id = url.rsplit('/').next()?;
    (id.len() > 1 && id.starts_with('Q') && id[1..].bytes().all(|c| c.is_ascii_digit()))
        .then(|| id.to_string())
}

/// `https://en.wikipedia.org/wiki/Title`, where only that is known.
fn wikipedia_page(v: &Value) -> Option<(String, String)> {
    let url = url_relation(v, "wikipedia")?;
    let rest = url.strip_prefix("https://")?;
    let (host, path) = rest.split_once('/')?;
    let lang = host.strip_suffix(".wikipedia.org")?;
    let title = path.strip_prefix("wiki/")?;
    Some((lang.to_string(), decode(title)))
}

fn sitelink(v: &Value, id: &str, language: &str) -> Option<(String, String)> {
    let links = v.get("entities").get(id).get("sitelinks");
    [language, "en"].iter().find_map(|lang| {
        let title = links.get(&format!("{lang}wiki")).get("title").as_str()?;
        Some((lang.to_string(), title.to_string()))
    })
}

fn read_image(v: &Value, a: &mut Artist) {
    let Some((_, page)) = v
        .get("query")
        .get("pages")
        .as_object()
        .and_then(|p| p.iter().next())
    else {
        return;
    };
    let info = page.get("imageinfo").at(0);
    let url = info.get("thumburl").as_str().or(info.get("url").as_str());
    let Some(url) = url.filter(|u| u.starts_with("https://")) else {
        return;
    };
    a.image_url = url.to_string();
    a.image_page = info.str_or("descriptionurl", "").to_string();
    let meta = info.get("extmetadata");
    let who = strip_html(meta.get("Artist").str_or("value", ""));
    let license = strip_html(meta.get("LicenseShortName").str_or("value", ""));
    a.image_credit = [who, license]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
}

fn read_similar(v: &Value) -> Vec<Related> {
    v.as_array()
        .into_iter()
        .flatten()
        .take(MOST_SIMILAR)
        .filter_map(|r| {
            Some(Related {
                name: r.get("name").as_str()?.to_string(),
                mbid: r.str_or("artist_mbid", "").to_string(),
                ..Related::default()
            })
        })
        .collect()
}

fn strip_html(text: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in text.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            c if !inside => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#039;", "'");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A value for Lucene's query syntax, between quotes.
fn query_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'%')
            .then(|| b.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(v) => {
                out.push(v);
                i += 3;
            }
            None => {
                out.push(if b[i] == b'_' { b' ' } else { b[i] });
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";

    // Answers cut down from what the services really said (the shape, not the
    // content).
    fn search(rows: &str) -> Vec<u8> {
        format!("{{\"created\":\"x\",\"count\":1,\"artists\":[{rows}]}}").into_bytes()
    }

    fn details() -> Vec<u8> {
        format!(
            r#"{{"id":"{ID}","name":"The Band","type":"Group","country":"GB",
            "area":{{"name":"United Kingdom"}},"begin-area":{{"name":"Abingdon"}},
            "life-span":{{"begin":"1991","end":null,"ended":false}},
            "genres":[{{"name":"art pop","count":3}},{{"name":"alternative rock","count":43}},{{"name":"art rock","count":30}}],
            "relations":[
              {{"type":"member of band","target-type":"artist","direction":"backward","begin":"1991","ended":false,"artist":{{"id":"11111111-1111-1111-1111-111111111111","name":"Colin"}}}},
              {{"type":"member of band","target-type":"artist","direction":"backward","begin":"1985","end":"1990","ended":true,"artist":{{"id":"22222222-2222-2222-2222-222222222222","name":"Old Member"}}}},
              {{"type":"member of band","target-type":"artist","direction":"backward","ended":false,"artist":{{"id":"11111111-1111-1111-1111-111111111111","name":"Colin"}}}},
              {{"type":"subgroup","target-type":"artist","direction":"forward","artist":{{"id":"33333333-3333-3333-3333-333333333333","name":"The Smile"}}}},
              {{"type":"official homepage","target-type":"url","direction":"forward","ended":false,"url":{{"resource":"http://www.example.org/"}}}},
              {{"type":"bandcamp","target-type":"url","direction":"forward","ended":false,"url":{{"resource":"https://band.bandcamp.com/"}}}},
              {{"type":"youtube","target-type":"url","direction":"forward","ended":true,"url":{{"resource":"https://www.youtube.com/channel/gone"}}}},
              {{"type":"wikidata","target-type":"url","direction":"forward","ended":false,"url":{{"resource":"https://www.wikidata.org/wiki/Q44190"}}}}
            ]}}"#
        )
        .into_bytes()
    }

    fn run(l: &mut Lookup, answers: &[(&str, u16, Vec<u8>)]) -> Vec<String> {
        let mut asked = Vec::new();
        for (part, status, body) in answers {
            let r = l.request().expect("a request");
            assert!(r.url.contains(part), "{} should contain {part}", r.url);
            asked.push(r.url);
            l.answer(*status, body).unwrap();
        }
        asked
    }

    #[test]
    fn a_whole_page_step_by_step() {
        let mut l = Lookup::new("the band", None, "de", Depth::Page);
        let asked = run(
            &mut l,
            &[
                (
                    "/artist?query=artist%3A%22the%20band%22",
                    200,
                    search(&format!(r#"{{"id":"{ID}","score":100,"name":"The Band"}}"#)),
                ),
                (&format!("/artist/{ID}?inc="), 200, details()),
                (
                    "wbgetentities&ids=Q44190",
                    200,
                    br#"{"entities":{"Q44190":{"sitelinks":{"dewiki":{"title":"The Band"},"enwiki":{"title":"The Band (group)"}}}}}"#.to_vec(),
                ),
                (
                    "wbgetclaims&entity=Q44190",
                    200,
                    br#"{"claims":{"P18":[{"mainsnak":{"datavalue":{"value":"Band live.jpg"}}}]}}"#.to_vec(),
                ),
                (
                    "https://de.wikipedia.org/api/rest_v1/page/summary/The_Band",
                    200,
                    br#"{"type":"standard","extract":" Eine Band. ","content_urls":{"desktop":{"page":"https://de.wikipedia.org/wiki/The_Band"}}}"#.to_vec(),
                ),
                (
                    "titles=File%3ABand%20live.jpg",
                    200,
                    br#"{"query":{"pages":{"1":{"imageinfo":[{"thumburl":"https://upload.wikimedia.org/x/640px-Band.jpg","url":"https://upload.wikimedia.org/x/Band.jpg","descriptionurl":"https://commons.wikimedia.org/wiki/File:Band.jpg","extmetadata":{"Artist":{"value":"<a href=\"x\">Some One</a>"},"LicenseShortName":{"value":"CC BY-SA 4.0"}}}]}}}}"#.to_vec(),
                ),
                (
                    "labs.api.listenbrainz.org/similar-artists/json?artist_mbids=",
                    200,
                    br#"[{"artist_mbid":"44444444-4444-4444-4444-444444444444","name":"Other Band","score":10}]"#.to_vec(),
                ),
            ],
        );
        assert_eq!(asked.len(), 7);
        assert_eq!(l.request(), None);
        let a = l.finish().unwrap();
        assert_eq!(a.name, "The Band");
        assert_eq!(a.kind, "group");
        assert_eq!(a.begin, "1991");
        assert!(!a.ended);
        assert!(!a.ambiguous);
        assert!(a.complete);
        assert_eq!(a.genres, ["alternative rock", "art rock", "art pop"]);
        assert_eq!(a.members.len(), 2, "one person once");
        assert_eq!(a.members[0].name, "Colin");
        assert!(a.members[1].ended);
        assert_eq!(a.biography, "Eine Band.");
        assert_eq!(a.biography_url, "https://de.wikipedia.org/wiki/The_Band");
        assert_eq!(a.image_url, "https://upload.wikimedia.org/x/640px-Band.jpg");
        assert_eq!(a.image_credit, "Some One, CC BY-SA 4.0");
        assert_eq!(a.similar[0].name, "Other Band");
        assert_eq!(a.links.len(), 2, "not the channel that is gone");
        assert_eq!(a.links[0].kind, "homepage");
    }

    #[test]
    fn genres_only_ask_musicbrainz_and_an_id_needs_no_search() {
        let mut l = Lookup::new("The Band", Some(ID), "de", Depth::Genres);
        let r = l.request().unwrap();
        assert_eq!(r.service, Service::MusicBrainz);
        assert!(r.url.contains(&format!("/artist/{ID}?")));
        l.answer(200, &details()).unwrap();
        assert_eq!(l.request(), None);
        let a = l.finish().unwrap();
        assert!(!a.complete);
        assert_eq!(a.genres[0], "alternative rock");
    }

    #[test]
    fn a_search_that_is_not_sure_finds_nobody() {
        for rows in [
            String::new(),
            format!(r#"{{"id":"{ID}","score":70,"name":"The Band"}}"#),
            format!(r#"{{"id":"{ID}","score":100,"name":"Another Name"}}"#),
            r#"{"id":"not-an-id","score":100,"name":"The Band"}"#.to_string(),
        ] {
            let mut l = Lookup::new("The Band", None, "de", Depth::Page);
            l.answer(200, &search(&rows)).unwrap();
            assert_eq!(l.request(), None, "{rows}");
            assert_eq!(l.finish(), None, "{rows}");
        }
    }

    #[test]
    fn names_are_compared_loosely_and_aliases_count() {
        let found = search(&format!(
            r#"{{"id":"{ID}","score":95,"name":"Die Ärzte","aliases":[{{"name":"Die Aerzte"}}]}}"#
        ));
        let v = bl_json::parse_bytes(&found).unwrap();
        assert!(confident(&v, "die ärzte").is_some());
        assert!(confident(&v, "DIE AERZTE").is_some());
        assert!(confident(&v, "Die Toten Hosen").is_none());
    }

    #[test]
    fn two_of_the_same_name_are_marked() {
        let mut l = Lookup::new("Twin", None, "de", Depth::Genres);
        l.answer(
            200,
            &search(&format!(
                r#"{{"id":"{ID}","score":100,"name":"Twin"}},{{"id":"55555555-5555-5555-5555-555555555555","score":100,"name":"Twin"}}"#
            )),
        )
        .unwrap();
        l.answer(200, &details()).unwrap();
        assert!(l.finish().unwrap().ambiguous);
    }

    #[test]
    fn a_service_that_is_down_leaves_nothing_to_remember() {
        for status in [0, 429, 500, 503] {
            let mut l = Lookup::new("The Band", None, "de", Depth::Page);
            let failed = l.answer(status, b"").unwrap_err();
            assert_eq!(failed.service, Service::MusicBrainz);
        }
        assert_eq!(
            Lookup::new("x", None, "de", Depth::Page)
                .answer(429, b"")
                .unwrap_err()
                .pause,
            30.0
        );
    }

    #[test]
    fn a_busy_moment_only_means_asking_again() {
        let mut l = Lookup::new("The Band", None, "de", Depth::Genres);
        let first = l.request().unwrap();
        assert!(l.answer(503, b"").is_err());
        assert_eq!(l.request().unwrap(), first, "the same question");
        l.answer(
            200,
            &search(&format!(r#"{{"id":"{ID}","score":100,"name":"The Band"}}"#)),
        )
        .unwrap();
        assert!(l.request().unwrap().url.contains(ID));
    }

    #[test]
    fn missing_icing_is_no_failure() {
        let mut l = Lookup::new("The Band", Some(ID), "de", Depth::Page);
        l.answer(200, &details()).unwrap();
        while l.request().is_some() {
            l.answer(500, b"").unwrap();
        }
        let a = l.finish().unwrap();
        assert_eq!(a.name, "The Band");
        assert!(a.biography.is_empty());
        assert!(a.image_url.is_empty());
    }

    #[test]
    fn nonsense_answers_do_no_harm() {
        let mut l = Lookup::new("The Band", None, "de", Depth::Page);
        l.answer(200, b"<html>").unwrap();
        assert_eq!(l.finish(), None);
        let mut l = Lookup::new("The Band", Some(ID), "de", Depth::Page);
        l.answer(404, b"").unwrap();
        assert_eq!(l.finish(), None);
        let mut l = Lookup::new("The Band", Some("not an id"), "de", Depth::Page);
        assert!(l.request().unwrap().url.contains("query="));
        l.answer(200, b"{}").unwrap();
        assert_eq!(l.finish(), None);
    }

    #[test]
    fn the_pace_is_kept() {
        let mut p = Pacer::default();
        assert!(p.ready(Service::MusicBrainz, 0.0));
        p.mark(Service::MusicBrainz, 10.0);
        assert!(!p.ready(Service::MusicBrainz, 10.5));
        assert!(p.ready(Service::Wikimedia, 10.5));
        assert!(p.ready(Service::MusicBrainz, 11.2));
        p.hold(Service::MusicBrainz, 20.0, 30.0);
        assert!(!p.ready(Service::MusicBrainz, 49.0));
    }

    #[test]
    fn query_characters_cannot_break_out() {
        let l = Lookup::new("AC/DC \"Live\" & co", None, "de", Depth::Genres);
        let url = l.request().unwrap().url;
        assert!(
            url.contains("AC%2FDC%20%5C%22Live%5C%22%20%26%20co"),
            "{url}"
        );
        assert_eq!(decode("Foo_Bar%C3%A4"), "Foo Barä");
    }
}
