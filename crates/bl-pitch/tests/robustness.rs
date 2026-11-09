//! Damaged UltraStar files and karaoke JSON: reading ends with a track or
//! an error; a track that reads writes back out without trouble.

#[path = "../../bl-json/tests/fuzz/mod.rs"]
mod fuzz;

use bl_pitch::ultrastar::{from_ultrastar_bytes, to_ultrastar};
use bl_pitch::KaraokeTrack;

const SONG: &str = "\
#TITLE:Song
#ARTIST:Someone
#LANGUAGE:English
#BPM:300
#GAP:1500,5
#MP3:song.mp3
#COVER:cover.jpg
: 0 4 5 Hel
: 4 4 7 lo
* 8 8 9 world
F 16 2 0 (hm)
- 20
R 22 4 2 rap
G 26 4 2 ped
- 32
: 34 4 -3 ~
E
";

const DUET: &str = "\
#TITLE:Duet
#BPM:200
#GAP:0
#RELATIVE:yes
#DUETSINGERP1:Ana
#DUETSINGERP2:Ben
P1
: 0 2 0 one
- 4 4
: 0 2 2 two
P2
: 8 2 4 three
- 12
: 14 2 5 four
P3
: 20 4 7 both
E
";

fn exercise_track(t: &KaraokeTrack) {
    for n in t.notes() {
        let _ = n.end_ms();
    }
    let _ = t.range();
    let _ = t.is_duet();
    let _ = bl_pitch::Key::estimate(t.notes());
    for l in &t.lines {
        let _ = (l.start_ms(), l.end_ms(), l.text());
    }
    let text = to_ultrastar(t);
    let json = t.to_json();
    let back = KaraokeTrack::from_json(&json).expect("written JSON reads");
    assert_eq!(back.lines.len(), t.lines.len());
    let _ = from_ultrastar_bytes(text.as_bytes());
}

#[test]
fn damaged_ultrastar_files_end_cleanly() {
    let samples = vec![SONG.as_bytes().to_vec(), DUET.as_bytes().to_vec()];
    let tokens: &[&[u8]] = &[
        b"\n: ",
        b"\n* ",
        b"\n- ",
        b"\nP",
        b"\nE\n",
        b"#BPM:",
        b"#GAP:",
        b"#RELATIVE:yes\n",
        b" 9223372036854775807",
        b" -9223372036854775808",
        b" 1e308",
        b",",
        b"\r",
        b"\xff",
        b"\xef\xbb\xbf",
    ];
    fuzz::run("ultrastar", &samples, tokens, 20_000, |d| {
        if let Ok(t) = from_ultrastar_bytes(d) {
            exercise_track(&t);
        }
    });
}

#[test]
fn damaged_karaoke_json_ends_cleanly() {
    let mut samples = Vec::new();
    for s in [SONG, DUET] {
        let mut t = bl_pitch::ultrastar::from_ultrastar(s).unwrap();
        t.estimate_key();
        samples.push(t.to_json().to_json_pretty().into_bytes());
    }
    let tokens: &[&[u8]] = &[
        b"{",
        b"}",
        b"[",
        b"]",
        b",",
        b"\"t\":",
        b"\"d\":",
        b"\"p\":",
        b"\"v\":",
        b"1e308",
        b"-1e308",
        b"9007199254740993",
        b"{\"notes\":[{\"t\":0,\"d\":1}]},",
        b"\"lines\":",
    ];
    fuzz::run("karaoke-json", &samples, tokens, 20_000, |d| {
        let Ok(v) = bl_json::parse_bytes(d) else {
            return;
        };
        if let Ok(t) = KaraokeTrack::from_json(&v) {
            exercise_track(&t);
        }
    });
}
