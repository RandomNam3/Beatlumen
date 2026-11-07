//! What is playing, as a status in the Discord program on the same computer
//! (Linux and Windows; off until asked for).
//!
//! Talks to Discord's local interface only (`discord-ipc-{n}`, on Windows
//! `\\.\pipe\discord-ipc-{n}`), with the id of an application the user
//! registered. No account token, no connection to Discord's servers, and it
//! does not pose as a Discord client. Only title, artist, album, start and
//! end time and the name of a picture of that application go out: no cover
//! links, they would give away a private server. Encrypted songs send nothing,
//! and pausing removes the status.
#![cfg_attr(
    not(any(windows, all(unix, not(target_os = "android")))),
    allow(dead_code)
)]

use crate::core::Core;
use crate::host::Host;
use crate::player::PlayState;
use bl_json::Value;

/// Name of the picture in the Discord application's Rich Presence assets.
const LOGO_ASSET: &str = "logo";

/// Seconds between looks at playback.
const CHECK_EVERY: f64 = 0.5;

/// A jump of the expected start by more than this was a seek.
const SEEK_TOLERANCE_MS: i64 = 2500;

/// Discord takes 2 to 128 characters per field, counted like JavaScript does.
const MIN_FIELD: usize = 2;
const MAX_FIELD: usize = 128;

/// What the status shows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Activity {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Unix time in milliseconds.
    pub start_ms: i64,
    pub end_ms: Option<i64>,
}

/// An application id: Discord's ids are decimal numbers of 17 to 20 digits.
pub(crate) fn valid_app_id(id: &str) -> bool {
    (17..=20).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())
}

/// Cut or fill a text to what Discord takes; empty stays empty (left out).
fn field(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let mut units = 0;
    for c in text.chars() {
        units += c.len_utf16();
        if units > MAX_FIELD {
            break;
        }
        out.push(c);
    }
    while out.encode_utf16().count() < MIN_FIELD {
        // A title of one letter would be refused otherwise.
        out.push('\u{2800}');
    }
    out
}

impl Activity {
    /// The `activity` of `SET_ACTIVITY`: type 2 is "Listening to".
    pub(crate) fn to_json(&self) -> Value {
        let mut timestamps = Value::object().with("start", self.start_ms);
        if let Some(end) = self.end_ms {
            timestamps = timestamps.with("end", end);
        }
        let mut assets = Value::object().with("large_image", LOGO_ASSET);
        let album = field(&self.album);
        if !album.is_empty() {
            assets = assets.with("large_text", album);
        }
        let mut activity = Value::object()
            .with("type", 2u64)
            .with("details", field(&self.title))
            .with("timestamps", timestamps)
            .with("assets", assets);
        let artist = field(&self.artist);
        if !artist.is_empty() {
            activity = activity.with("state", artist);
        }
        activity
    }
}

/// The connection, in a thread of its own: Discord may come and go at any
/// time, and the app must never wait for it.
#[cfg(any(windows, all(unix, not(target_os = "android"))))]
mod ipc {
    use super::Activity;
    use bl_json::Value;
    use std::io::{self, Read, Write};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    const OP_HANDSHAKE: u32 = 0;
    const OP_FRAME: u32 = 1;
    const OP_CLOSE: u32 = 2;
    const OP_PING: u32 = 3;
    const OP_PONG: u32 = 4;

    /// Nothing Discord sends here comes close to this.
    const MAX_FRAME: usize = 1 << 20;
    /// Seconds until Discord is looked for again.
    const RETRY: Duration = Duration::from_secs(15);
    /// How long a refused application id is left alone.
    const RETRY_REFUSED: Duration = Duration::from_secs(300);
    const READY_TIMEOUT: Duration = Duration::from_secs(10);
    const POLL: Duration = Duration::from_millis(200);
    /// Discord allows five updates in 20 seconds; the last one wins.
    const MIN_GAP: Duration = Duration::from_secs(4);

    /// A frame: operation and length, both 32 bits, little endian, then JSON.
    pub(super) fn encode(op: u32, body: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + body.len());
        out.extend_from_slice(&op.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body.as_bytes());
        out
    }

    /// The first whole frame in `buf` and its length.
    pub(super) fn decode(buf: &[u8]) -> io::Result<Option<(u32, Vec<u8>, usize)>> {
        if buf.len() < 8 {
            return Ok(None);
        }
        let op = u32::from_le_bytes(buf[..4].try_into().expect("four bytes"));
        let len = u32::from_le_bytes(buf[4..8].try_into().expect("four bytes")) as usize;
        if len > MAX_FRAME {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too long"));
        }
        if buf.len() < 8 + len {
            return Ok(None);
        }
        Ok(Some((op, buf[8..8 + len].to_vec(), 8 + len)))
    }

    /// The open connection. `read_some` never waits: on Windows a read that
    /// waits on a pipe holds up every write on the same handle.
    pub(super) trait Pipe {
        fn write_frame(&mut self, data: &[u8]) -> io::Result<()>;
        fn read_some(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    }

    #[cfg(unix)]
    impl Pipe for std::os::unix::net::UnixStream {
        fn write_frame(&mut self, data: &[u8]) -> io::Result<()> {
            self.write_all(data)
        }
        fn read_some(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.read(buf) {
                Ok(0) => Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => Ok(n),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    Ok(0)
                }
                Err(e) => Err(e),
            }
        }
    }

    /// The places Discord's socket can be: the first folder that has the
    /// variable set, plain or as Flatpak or Snap keep it.
    #[cfg(unix)]
    pub(super) fn candidates() -> Vec<std::path::PathBuf> {
        use std::path::PathBuf;
        let base = ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"]
            .iter()
            .find_map(|v| std::env::var_os(v).filter(|d| !d.is_empty()))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let mut out = Vec::new();
        for sub in ["", "app/com.discordapp.Discord", "snap.discord"] {
            for n in 0..10 {
                out.push(base.join(sub).join(format!("discord-ipc-{n}")));
            }
        }
        out
    }

    #[cfg(unix)]
    pub(super) fn connect_at(path: &std::path::Path) -> io::Result<Box<dyn Pipe + Send>> {
        let s = std::os::unix::net::UnixStream::connect(path)?;
        s.set_read_timeout(Some(Duration::from_millis(1)))?;
        s.set_write_timeout(Some(Duration::from_secs(2)))?;
        Ok(Box::new(s))
    }

    #[cfg(windows)]
    mod win {
        use super::Pipe;
        use std::fs::File;
        use std::io::{self, Read, Write};
        use std::os::windows::io::AsRawHandle;

        extern "system" {
            fn PeekNamedPipe(
                pipe: *mut core::ffi::c_void,
                buffer: *mut core::ffi::c_void,
                size: u32,
                read: *mut u32,
                available: *mut u32,
                left: *mut u32,
            ) -> i32;
        }

        pub(super) struct NamedPipe(pub File);

        impl Pipe for NamedPipe {
            fn write_frame(&mut self, data: &[u8]) -> io::Result<()> {
                self.0.write_all(data)
            }
            fn read_some(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let mut available = 0u32;
                // SAFETY: the handle lives as long as `self.0`; no buffer is
                // passed, so only `available` is written.
                let ok = unsafe {
                    PeekNamedPipe(
                        self.0.as_raw_handle() as *mut _,
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null_mut(),
                        &mut available,
                        std::ptr::null_mut(),
                    )
                };
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                if available == 0 {
                    return Ok(0);
                }
                let n = buf.len().min(available as usize);
                self.0.read(&mut buf[..n])
            }
        }
    }

    #[cfg(windows)]
    pub(super) fn candidates() -> Vec<std::path::PathBuf> {
        (0..10)
            .map(|n| std::path::PathBuf::from(format!(r"\\.\pipe\discord-ipc-{n}")))
            .collect()
    }

    #[cfg(windows)]
    pub(super) fn connect_at(path: &std::path::Path) -> io::Result<Box<dyn Pipe + Send>> {
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        Ok(Box::new(win::NamedPipe(f)))
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Link {
        /// Discord does not run here (or not yet).
        Waiting,
        Connected,
        /// Discord does not know the application id.
        Refused,
    }

    enum Cmd {
        Set(Option<Activity>),
    }

    /// The running connection; dropping it ends the thread, and Discord
    /// removes the status when the connection closes.
    pub(crate) struct Client {
        app_id: String,
        cmd: Sender<Cmd>,
        link: Arc<Mutex<Link>>,
    }

    impl Client {
        pub(crate) fn start(app_id: &str, places: Vec<std::path::PathBuf>) -> Client {
            let (cmd, rx) = mpsc::channel();
            let link = Arc::new(Mutex::new(Link::Waiting));
            let worker = Worker {
                app_id: app_id.to_string(),
                places,
                rx,
                link: link.clone(),
                wanted: None,
                nonce: 0,
            };
            let _ = std::thread::Builder::new()
                .name("discord".into())
                .spawn(move || worker.run());
            Client {
                app_id: app_id.to_string(),
                cmd,
                link,
            }
        }

        pub(crate) fn app_id(&self) -> &str {
            &self.app_id
        }

        pub(crate) fn set(&self, activity: Option<Activity>) {
            let _ = self.cmd.send(Cmd::Set(activity));
        }

        pub(crate) fn link(&self) -> Link {
            self.link.lock().map(|l| *l).unwrap_or(Link::Waiting)
        }
    }

    struct Worker {
        app_id: String,
        places: Vec<std::path::PathBuf>,
        rx: Receiver<Cmd>,
        link: Arc<Mutex<Link>>,
        wanted: Option<Activity>,
        nonce: u64,
    }

    /// Why a connection ended.
    enum End {
        /// The app no longer wants one.
        Stop,
        /// Discord went away or broke the protocol.
        Lost,
        Refused,
    }

    impl Worker {
        fn set_link(&self, link: Link) {
            if let Ok(mut l) = self.link.lock() {
                *l = link;
            }
        }

        fn run(mut self) {
            loop {
                let wait = match self.connect() {
                    Some(mut pipe) => match self.session(pipe.as_mut()) {
                        End::Stop => return,
                        End::Lost => {
                            self.set_link(Link::Waiting);
                            RETRY
                        }
                        End::Refused => {
                            self.set_link(Link::Refused);
                            RETRY_REFUSED
                        }
                    },
                    None => RETRY,
                };
                if !self.idle(wait) {
                    return;
                }
            }
        }

        /// The first socket that answers; a file left behind by a Discord
        /// that crashed refuses the connection.
        fn connect(&self) -> Option<Box<dyn Pipe + Send>> {
            self.places.iter().find_map(|p| connect_at(p).ok())
        }

        /// Waits without a connection; false when the app no longer wants one.
        fn idle(&mut self, wait: Duration) -> bool {
            let until = Instant::now() + wait;
            loop {
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return true;
                }
                match self.rx.recv_timeout(left) {
                    Ok(Cmd::Set(a)) => self.wanted = a,
                    Err(RecvTimeoutError::Timeout) => return true,
                    Err(RecvTimeoutError::Disconnected) => return false,
                }
            }
        }

        fn session(&mut self, pipe: &mut dyn Pipe) -> End {
            let hello = Value::object()
                .with("v", 1u64)
                .with("client_id", self.app_id.as_str())
                .to_json();
            if pipe.write_frame(&encode(OP_HANDSHAKE, &hello)).is_err() {
                return End::Lost;
            }
            let mut buf = Vec::new();
            let mut ready = false;
            let started = Instant::now();
            // What Discord has been told, and when. A new connection shows
            // nothing yet.
            let mut told: Option<Activity> = None;
            let mut last_sent: Option<Instant> = None;
            loop {
                if !ready && started.elapsed() > READY_TIMEOUT {
                    return End::Lost;
                }
                match self.rx.recv_timeout(POLL) {
                    Ok(Cmd::Set(a)) => self.wanted = a,
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => {
                        let _ = pipe.write_frame(&encode(OP_CLOSE, "{}"));
                        return End::Stop;
                    }
                }
                match self.read_frames(pipe, &mut buf) {
                    Ok(frames) => {
                        for (op, body) in frames {
                            match op {
                                OP_FRAME => {
                                    let Some(msg) = std::str::from_utf8(&body)
                                        .ok()
                                        .and_then(|s| bl_json::parse(s).ok())
                                    else {
                                        continue;
                                    };
                                    if msg.str_or("evt", "") == "READY" {
                                        ready = true;
                                        self.set_link(Link::Connected);
                                    }
                                }
                                OP_PING => {
                                    if pipe.write_frame(&encode(OP_PONG, &text(&body))).is_err() {
                                        return End::Lost;
                                    }
                                }
                                OP_CLOSE => {
                                    return if ready { End::Lost } else { End::Refused };
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(_) => return End::Lost,
                }
                let due = last_sent.is_none_or(|t| t.elapsed() >= MIN_GAP);
                if ready && due && told != self.wanted {
                    let wanted = self.wanted.clone();
                    if self.send(pipe, wanted.as_ref()).is_err() {
                        return End::Lost;
                    }
                    told = wanted;
                    last_sent = Some(Instant::now());
                }
            }
        }

        fn read_frames(
            &self,
            pipe: &mut dyn Pipe,
            buf: &mut Vec<u8>,
        ) -> io::Result<Vec<(u32, Vec<u8>)>> {
            let mut chunk = [0u8; 4096];
            loop {
                let n = pipe.read_some(&mut chunk)?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let mut frames = Vec::new();
            while let Some((op, body, used)) = decode(buf)? {
                buf.drain(..used);
                frames.push((op, body));
            }
            Ok(frames)
        }

        /// `SET_ACTIVITY`; without an activity it clears the status.
        fn send(&mut self, pipe: &mut dyn Pipe, activity: Option<&Activity>) -> io::Result<()> {
            self.nonce += 1;
            let mut args = Value::object().with("pid", u64::from(std::process::id()));
            if let Some(a) = activity {
                args = args.with("activity", a.to_json());
            }
            let msg = Value::object()
                .with("cmd", "SET_ACTIVITY")
                .with("args", args)
                .with("nonce", self.nonce.to_string())
                .to_json();
            pipe.write_frame(&encode(OP_FRAME, &msg))
        }
    }

    fn text(body: &[u8]) -> String {
        String::from_utf8_lossy(body).into_owned()
    }
}

#[cfg(any(windows, all(unix, not(target_os = "android"))))]
pub(crate) const AVAILABLE: bool = true;
#[cfg(not(any(windows, all(unix, not(target_os = "android")))))]
pub(crate) const AVAILABLE: bool = false;

#[derive(Default)]
pub(crate) struct DiscordState {
    #[cfg(any(windows, all(unix, not(target_os = "android"))))]
    client: Option<ipc::Client>,
    /// Song and expected start and end of what was last handed on.
    shown: Option<(String, i64, Option<i64>)>,
    checked_at: f64,
    /// Sockets to use instead of Discord's real ones.
    #[cfg(test)]
    places: Vec<std::path::PathBuf>,
}

/// What playback means for the status.
#[derive(Debug, PartialEq)]
enum Wanted {
    Show(Activity),
    Clear,
    /// A song is loading: the status stays until it plays.
    Keep,
}

fn wanted(
    track: &bl_net::api::Track,
    state: PlayState,
    position: f64,
    duration: Option<f64>,
    unix_time: f64,
) -> Wanted {
    match state {
        PlayState::Stopped | PlayState::Paused => Wanted::Clear,
        PlayState::Loading => Wanted::Keep,
        PlayState::Playing => {
            let start_ms = ((unix_time - position) * 1000.0) as i64;
            let duration = duration.unwrap_or(track.duration_ms as f64 / 1000.0);
            Wanted::Show(Activity {
                title: track.title.clone(),
                artist: track.artists.join(", "),
                album: track.album.clone(),
                start_ms,
                end_ms: (duration > 0.0).then(|| start_ms + (duration * 1000.0) as i64),
            })
        }
    }
}

/// Another song, or the times moved by more than the clocks' jitter: a seek,
/// or the computer slept.
fn discord_changed(
    shown: &Option<(String, i64, Option<i64>)>,
    new: &Option<(String, i64, Option<i64>)>,
) -> bool {
    match (shown, new) {
        (None, None) => false,
        (Some((a, start_a, end_a)), Some((b, start_b, end_b))) => {
            a != b
                || (start_a - start_b).abs() > SEEK_TOLERANCE_MS
                || match (end_a, end_b) {
                    (Some(x), Some(y)) => (x - y).abs() > SEEK_TOLERANCE_MS,
                    (None, None) => false,
                    _ => true,
                }
        }
        _ => true,
    }
}

impl Core {
    /// Whether a song may show up in the status: encrypted songs never, and
    /// neither what is on its way to the encrypted files or was sent here
    /// by someone else (AirPlay, DLNA, another program).
    fn discord_may_show(&self, id: &str) -> bool {
        !id.starts_with(crate::vault::ID_PREFIX)
            && !id.starts_with("mem:")
            && !id.starts_with(crate::capture::ID_PREFIX)
            && !id.starts_with(crate::airplay::ID_PREFIX)
            && !id.starts_with(crate::dlna_receiver::ID_PREFIX)
            && !self.extension_job_is_private(id)
    }

    fn discord_wanted(&self, host: &dyn Host) -> Wanted {
        let Some(track) = self.current.as_ref() else {
            return Wanted::Clear;
        };
        if !self.discord_may_show(&track.id) {
            return Wanted::Clear;
        }
        wanted(
            track,
            self.player.state(),
            self.player.position(host),
            self.player.duration(),
            host.unix_time(),
        )
    }

    /// Keeps the status in step with playback.
    #[cfg(any(windows, all(unix, not(target_os = "android"))))]
    pub(crate) fn poll_discord(&mut self, host: &dyn Host) {
        let id = &self.settings.discord_app_id;
        if !self.settings.discord_status || !valid_app_id(id) {
            self.discord.client = None;
            self.discord.shown = None;
            return;
        }
        if self.discord.client.as_ref().map(ipc::Client::app_id) != Some(id.as_str()) {
            #[cfg(test)]
            let places = self.discord.places.clone();
            #[cfg(not(test))]
            let places = ipc::candidates();
            self.discord.client = Some(ipc::Client::start(id, places));
            self.discord.shown = None;
        }
        if (self.time - self.discord.checked_at).abs() < CHECK_EVERY {
            return;
        }
        self.discord.checked_at = self.time;
        let activity = match self.discord_wanted(host) {
            Wanted::Keep => return,
            Wanted::Clear => None,
            Wanted::Show(a) => Some(a),
        };
        let new = activity.as_ref().map(|a| {
            let track = self.current.as_ref().map(|t| t.id.clone());
            (track.unwrap_or_default(), a.start_ms, a.end_ms)
        });
        if discord_changed(&self.discord.shown, &new) {
            self.discord.shown = new;
            if let Some(client) = &self.discord.client {
                client.set(activity);
            }
        }
    }

    #[cfg(not(any(windows, all(unix, not(target_os = "android")))))]
    pub(crate) fn poll_discord(&mut self, _host: &dyn Host) {}

    /// For the settings page.
    pub(crate) fn discord_json(&self) -> Value {
        let state = if !AVAILABLE {
            "unavailable"
        } else if !self.settings.discord_status {
            "off"
        } else if !valid_app_id(&self.settings.discord_app_id) {
            "noId"
        } else {
            self.discord_link()
        };
        Value::object()
            .with("available", AVAILABLE)
            .with("state", state)
    }

    #[cfg(any(windows, all(unix, not(target_os = "android"))))]
    fn discord_link(&self) -> &'static str {
        match self.discord.client.as_ref().map(ipc::Client::link) {
            Some(ipc::Link::Connected) => "connected",
            Some(ipc::Link::Refused) => "refused",
            Some(ipc::Link::Waiting) | None => "waiting",
        }
    }

    #[cfg(not(any(windows, all(unix, not(target_os = "android")))))]
    fn discord_link(&self) -> &'static str {
        "unavailable"
    }
}

#[cfg(all(test, any(windows, unix)))]
mod tests {
    use super::ipc::{decode, encode, Client, Link};
    use super::*;
    use crate::artists::tests::TestHost;
    use std::io::{Read, Write};
    #[cfg(unix)]
    use std::os::unix::net::{UnixListener, UnixStream};
    #[cfg(unix)]
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    const APP: &str = "123456789012345678";

    fn activity(title: &str) -> Activity {
        Activity {
            title: title.into(),
            artist: "Artist".into(),
            album: "Album".into(),
            start_ms: 1_800_000_000_000,
            end_ms: Some(1_800_000_200_000),
        }
    }

    #[cfg(unix)]
    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bl-discord-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Plays Discord's side of the socket or pipe.
    struct FakeDiscord<S> {
        stream: S,
    }

    #[cfg(unix)]
    impl FakeDiscord<UnixStream> {
        fn accept(listener: &UnixListener) -> Self {
            listener.set_nonblocking(false).unwrap();
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(15)))
                .unwrap();
            FakeDiscord { stream }
        }
    }

    impl<S: Read + Write> FakeDiscord<S> {
        fn read_frame(&mut self) -> (u32, Value) {
            let mut head = [0u8; 8];
            self.stream.read_exact(&mut head).unwrap();
            let op = u32::from_le_bytes(head[..4].try_into().unwrap());
            let len = u32::from_le_bytes(head[4..].try_into().unwrap()) as usize;
            let mut body = vec![0u8; len];
            self.stream.read_exact(&mut body).unwrap();
            (
                op,
                bl_json::parse(std::str::from_utf8(&body).unwrap()).unwrap(),
            )
        }

        fn write_frame(&mut self, op: u32, body: &str) {
            self.stream.write_all(&encode(op, body)).unwrap();
        }

        fn ready(&mut self) {
            let (op, hello) = self.read_frame();
            assert_eq!(op, 0, "first the handshake");
            assert_eq!(hello.get("v").as_f64(), Some(1.0));
            assert_eq!(hello.str_or("client_id", ""), APP);
            self.write_frame(1, r#"{"cmd":"DISPATCH","evt":"READY","data":{"v":1}}"#);
        }
    }

    fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(20);
        while !ok() {
            assert!(Instant::now() < until, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn track(id: &str) -> bl_net::api::Track {
        bl_net::api::Track {
            id: id.into(),
            title: "Song".into(),
            artists: vec!["One".into(), "Two".into()],
            album: "Album".into(),
            duration_ms: 200_000,
            ..Default::default()
        }
    }

    #[test]
    fn playing_shows_the_times_and_pausing_clears() {
        let t = track("abc");
        let Wanted::Show(a) = wanted(&t, PlayState::Playing, 10.0, None, 1000.0) else {
            panic!("should show");
        };
        assert_eq!(a.title, "Song");
        assert_eq!(a.artist, "One, Two");
        assert_eq!(a.album, "Album");
        assert_eq!(a.start_ms, 990_000);
        assert_eq!(a.end_ms, Some(1_190_000));
        // The decoder knows better than the tags.
        let Wanted::Show(a) = wanted(&t, PlayState::Playing, 0.0, Some(100.0), 1000.0) else {
            panic!("should show");
        };
        assert_eq!(a.end_ms, Some(1_100_000));
        assert_eq!(
            wanted(&t, PlayState::Paused, 10.0, None, 1000.0),
            Wanted::Clear
        );
        assert_eq!(
            wanted(&t, PlayState::Stopped, 0.0, None, 1000.0),
            Wanted::Clear
        );
        assert_eq!(
            wanted(&t, PlayState::Loading, 0.0, None, 1000.0),
            Wanted::Keep
        );
    }

    #[test]
    fn no_length_means_no_end_time() {
        let t = bl_net::api::Track {
            duration_ms: 0,
            ..track("radio:1")
        };
        let Wanted::Show(a) = wanted(&t, PlayState::Playing, 0.0, None, 1000.0) else {
            panic!("should show");
        };
        assert_eq!(a.end_ms, None);
    }

    #[test]
    fn only_a_seek_or_another_song_counts_as_a_change() {
        let shown = Some(("a".to_string(), 10_000, Some(20_000)));
        let same = |start, end| Some(("a".to_string(), start, Some(end)));
        assert!(!discord_changed(&None, &None));
        assert!(discord_changed(&None, &shown));
        assert!(discord_changed(&shown, &None));
        assert!(!discord_changed(&shown, &same(10_400, 20_400)), "jitter");
        assert!(discord_changed(&shown, &same(40_000, 50_000)), "seek");
        assert!(
            discord_changed(&shown, &same(10_000, 90_000)),
            "real length"
        );
        assert!(discord_changed(
            &shown,
            &Some(("b".to_string(), 10_000, Some(20_000)))
        ));
        assert!(discord_changed(
            &shown,
            &Some(("a".to_string(), 10_000, None))
        ));
    }

    fn core() -> (crate::Core, TestHost) {
        let mut host = TestHost::default();
        let core = crate::Core::new(&mut host, crate::LaunchArgs::default(), "de");
        (core, host)
    }

    #[test]
    fn encrypted_and_received_songs_never_show() {
        let (core, _host) = core();
        assert!(core.discord_may_show("abc"), "the server's songs");
        assert!(core.discord_may_show("file:/music/a.flac"), "local files");
        for id in [
            "vault:1",
            "mem:a",
            "airplay:1",
            "dlna:1",
            "capture:1",
            "ext:unknown",
        ] {
            assert!(!core.discord_may_show(id), "{id}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn off_at_first_and_without_an_id_nothing_connects() {
        let (mut core, mut host) = core();
        assert!(!core.settings.discord_status, "off from the start");
        let dir = folder("settings");
        let path = dir.join("discord-ipc-0");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        core.discord.places = vec![path];
        core.settings.discord_app_id = APP.into();
        core.current = Some(track("abc"));
        core.tick(&mut host);
        assert!(core.discord.client.is_none(), "switch off");
        assert_eq!(core.discord_json().str_or("state", ""), "off");

        core.settings.discord_status = true;
        core.settings.discord_app_id = "not-an-id".into();
        core.tick(&mut host);
        assert!(core.discord.client.is_none(), "invalid id");
        assert_eq!(core.discord_json().str_or("state", ""), "noId");
        std::thread::sleep(Duration::from_millis(100));
        assert!(listener.accept().is_err(), "nobody knocked");

        core.settings.discord_app_id = APP.into();
        core.tick(&mut host);
        assert!(core.discord.client.is_some());
        let mut discord = FakeDiscord::accept(&listener);
        discord.ready();
        wait_for("connected", || {
            core.discord_json().str_or("state", "") == "connected"
        });

        // Switched off, the connection goes and the status with it.
        core.settings.discord_status = false;
        core.tick(&mut host);
        assert!(core.discord.client.is_none());
        let (op, _) = discord.read_frame();
        assert_eq!(op, 2, "a clean goodbye");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_settings_keep_digits_only() {
        let s = crate::settings::Settings::from_json(
            &bl_json::parse(r#"{"discordStatus":true,"discordAppId":" 12x34 "}"#).unwrap(),
        );
        assert!(s.discord_status);
        assert_eq!(s.discord_app_id, "1234");
        let d = crate::settings::Settings::from_json(&bl_json::parse("{}").unwrap());
        assert!(!d.discord_status, "off unless asked for");
        assert_eq!(d.discord_app_id, "", "no id built in");
    }

    #[test]
    fn frames_survive_the_round_trip_and_arrive_in_pieces() {
        let frame = encode(1, r#"{"a":"ü"}"#);
        assert_eq!(&frame[..4], &1u32.to_le_bytes());
        assert_eq!(&frame[4..8], &10u32.to_le_bytes());
        assert!(decode(&frame[..5]).unwrap().is_none(), "head is cut");
        assert!(decode(&frame[..9]).unwrap().is_none(), "body is cut");
        let mut two = frame.clone();
        two.extend_from_slice(&encode(3, "{}"));
        let (op, body, used) = decode(&two).unwrap().unwrap();
        assert_eq!((op, body.as_slice(), used), (1, &frame[8..], frame.len()));
        let (op, _, _) = decode(&two[used..]).unwrap().unwrap();
        assert_eq!(op, 3);
    }

    #[test]
    fn a_frame_claiming_to_be_huge_is_refused_without_waiting_for_it() {
        let mut bad = 1u32.to_le_bytes().to_vec();
        bad.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode(&bad).is_err());
    }

    #[test]
    fn only_what_was_promised_goes_out() {
        let json = activity("Song").to_json().to_json();
        let v = bl_json::parse(&json).unwrap();
        assert_eq!(v.get("type").as_f64(), Some(2.0), "listening");
        assert_eq!(v.str_or("details", ""), "Song");
        assert_eq!(v.str_or("state", ""), "Artist");
        assert_eq!(v.get("timestamps").get("start").as_f64(), Some(1.8e12));
        assert_eq!(
            v.get("timestamps").get("end").as_f64(),
            Some(1_800_000_200_000.0)
        );
        assert_eq!(v.get("assets").str_or("large_image", ""), "logo");
        assert_eq!(v.get("assets").str_or("large_text", ""), "Album");
        for key in ["buttons", "party", "secrets", "instance", "url", "name"] {
            assert!(v.get(key).is_null(), "{key} must stay out: {json}");
        }
        assert!(!json.contains("http"), "no links at all: {json}");
        assert!(
            json.contains("\"start\":1800000000000"),
            "a plain integer: {json}"
        );
    }

    #[test]
    fn empty_fields_stay_out_and_no_end_without_a_length() {
        let v = Activity {
            artist: String::new(),
            album: String::new(),
            end_ms: None,
            ..activity("Song")
        }
        .to_json();
        assert!(v.get("state").is_null());
        assert!(v.get("assets").get("large_text").is_null());
        assert!(v.get("timestamps").get("end").is_null());
    }

    #[test]
    fn fields_fit_what_discord_takes() {
        assert_eq!(field("  "), "");
        assert_eq!(field("A").encode_utf16().count(), 2, "too short is filled");
        assert_eq!(field("Ab"), "Ab");
        let long = "x".repeat(300);
        assert_eq!(field(&long).encode_utf16().count(), MAX_FIELD);
        // Emoji count double, and none is cut in half.
        let emoji = "🎵".repeat(100);
        let cut = field(&emoji);
        assert_eq!(cut.encode_utf16().count(), MAX_FIELD);
        assert_eq!(cut.chars().count(), MAX_FIELD / 2);
    }

    #[cfg(unix)]
    #[test]
    fn it_looks_where_discord_and_its_packages_put_the_socket() {
        let places = super::ipc::candidates();
        let names: Vec<String> = places.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(names.len(), 30);
        assert!(names.iter().any(|n| n.ends_with("/discord-ipc-0")));
        assert!(names.iter().any(|n| n.ends_with("/discord-ipc-9")));
        assert!(names
            .iter()
            .any(|n| n.ends_with("app/com.discordapp.Discord/discord-ipc-0")));
        assert!(names
            .iter()
            .any(|n| n.ends_with("snap.discord/discord-ipc-0")));
    }

    #[test]
    fn only_decimal_ids_of_the_right_length_are_ids() {
        assert!(valid_app_id(APP));
        assert!(!valid_app_id(""));
        assert!(!valid_app_id("1234"));
        assert!(!valid_app_id("12345678901234567a"));
        assert!(!valid_app_id("../../../etc/passwd"));
    }

    #[cfg(unix)]
    #[test]
    fn handshake_status_and_clearing_over_a_real_socket() {
        let dir = folder("flow");
        let path = dir.join("discord-ipc-0");
        let listener = UnixListener::bind(&path).unwrap();
        let client = Client::start(APP, vec![path]);
        client.set(Some(activity("First")));
        let mut discord = FakeDiscord::accept(&listener);
        discord.ready();
        wait_for("connected", || client.link() == Link::Connected);

        let (op, msg) = discord.read_frame();
        assert_eq!(op, 1);
        assert_eq!(msg.str_or("cmd", ""), "SET_ACTIVITY");
        assert_eq!(
            msg.get("args").get("pid").as_f64(),
            Some(std::process::id() as f64)
        );
        assert_eq!(
            msg.get("args").get("activity").str_or("details", ""),
            "First"
        );
        assert!(!msg.str_or("nonce", "").is_empty());

        client.set(None);
        let (_, msg) = discord.read_frame();
        assert_eq!(msg.str_or("cmd", ""), "SET_ACTIVITY");
        assert!(msg.get("args").get("activity").is_null(), "cleared");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_ping_gets_its_pong() {
        let dir = folder("ping");
        let path = dir.join("discord-ipc-0");
        let listener = UnixListener::bind(&path).unwrap();
        let client = Client::start(APP, vec![path]);
        let mut discord = FakeDiscord::accept(&listener);
        discord.ready();
        discord.write_frame(3, r#"{"n":7}"#);
        let (op, body) = discord.read_frame();
        assert_eq!(op, 4);
        assert_eq!(body.get("n").as_f64(), Some(7.0));
        drop(client);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn updates_in_a_burst_end_on_the_last_one() {
        let dir = folder("burst");
        let path = dir.join("discord-ipc-0");
        let listener = UnixListener::bind(&path).unwrap();
        let client = Client::start(APP, vec![path]);
        let mut discord = FakeDiscord::accept(&listener);
        discord.ready();
        wait_for("connected", || client.link() == Link::Connected);
        client.set(Some(activity("One")));
        let (_, msg) = discord.read_frame();
        assert_eq!(msg.get("args").get("activity").str_or("details", ""), "One");
        let sent = Instant::now();
        for title in ["Two", "Three", "Four"] {
            client.set(Some(activity(title)));
        }
        let (_, msg) = discord.read_frame();
        assert_eq!(
            msg.get("args").get("activity").str_or("details", ""),
            "Four"
        );
        assert!(sent.elapsed() >= Duration::from_secs(3), "keeps the pace");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_id_is_reported_and_not_hammered() {
        let dir = folder("refused");
        let path = dir.join("discord-ipc-0");
        let listener = UnixListener::bind(&path).unwrap();
        let client = Client::start(APP, vec![path]);
        let mut discord = FakeDiscord::accept(&listener);
        let _ = discord.read_frame();
        discord.write_frame(2, r#"{"code":4000,"message":"Invalid Client ID"}"#);
        wait_for("refused", || client.link() == Link::Refused);
        listener.set_nonblocking(true).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert!(listener.accept().is_err(), "no second try right away");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_socket_left_behind_is_passed_over() {
        let dir = folder("stale");
        // A file that stayed when Discord crashed: nobody listens on it.
        let stale = dir.join("discord-ipc-0");
        drop(UnixListener::bind(&stale).unwrap());
        assert!(stale.exists());
        let live = dir.join("discord-ipc-1");
        let listener = UnixListener::bind(&live).unwrap();
        let client = Client::start(APP, vec![stale, live]);
        let mut discord = FakeDiscord::accept(&listener);
        discord.ready();
        wait_for("connected", || client.link() == Link::Connected);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn it_waits_quietly_while_discord_does_not_run() {
        let dir = folder("absent");
        let client = Client::start(APP, vec![dir.join("discord-ipc-0")]);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(client.link(), Link::Waiting);
        let started = Instant::now();
        drop(client);
        // Dropping does not wait for the thread, and the thread sees it.
        assert!(started.elapsed() < Duration::from_millis(100));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn discord_going_away_is_noticed() {
        let dir = folder("gone");
        let path = dir.join("discord-ipc-0");
        let listener = UnixListener::bind(&path).unwrap();
        let client = Client::start(APP, vec![path]);
        let mut discord = FakeDiscord::accept(&listener);
        discord.ready();
        wait_for("connected", || client.link() == Link::Connected);
        drop(discord);
        wait_for("waiting again", || client.link() == Link::Waiting);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The only place Windows' side runs for real: the CI's Windows job.
    #[cfg(windows)]
    mod pipe {
        use super::*;
        use std::fs::File;
        use std::os::windows::io::{FromRawHandle, RawHandle};

        extern "system" {
            fn CreateNamedPipeW(
                name: *const u16,
                open_mode: u32,
                pipe_mode: u32,
                instances: u32,
                out_size: u32,
                in_size: u32,
                timeout: u32,
                security: *mut core::ffi::c_void,
            ) -> *mut core::ffi::c_void;
            fn ConnectNamedPipe(
                pipe: *mut core::ffi::c_void,
                overlapped: *mut core::ffi::c_void,
            ) -> i32;
        }

        const PIPE_ACCESS_DUPLEX: u32 = 3;

        /// Opens a pipe; the handle goes to the thread that waits on it.
        fn serve(name: &str) -> std::thread::JoinHandle<FakeDiscord<File>> {
            let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
            // SAFETY: `wide` ends in a zero and outlives the call.
            let raw = unsafe {
                CreateNamedPipeW(
                    wide.as_ptr(),
                    PIPE_ACCESS_DUPLEX,
                    0,
                    1,
                    4096,
                    4096,
                    0,
                    std::ptr::null_mut(),
                )
            } as usize;
            assert!(raw != 0 && raw != usize::MAX, "cannot create the pipe");
            std::thread::spawn(move || {
                // SAFETY: the handle is valid, and only this thread uses it.
                let file = unsafe {
                    ConnectNamedPipe(raw as *mut _, std::ptr::null_mut());
                    File::from_raw_handle(raw as RawHandle)
                };
                FakeDiscord { stream: file }
            })
        }

        #[test]
        fn handshake_status_and_clearing_over_a_real_pipe() {
            let name = format!(r"\\.\pipe\bl-discord-test-{}", std::process::id());
            let server = serve(&name);
            let client = Client::start(APP, vec![std::path::PathBuf::from(&name)]);
            client.set(Some(activity("First")));
            let mut discord = server.join().unwrap();
            discord.ready();
            wait_for("connected", || client.link() == Link::Connected);

            let (op, msg) = discord.read_frame();
            assert_eq!(op, 1);
            assert_eq!(
                msg.get("args").get("activity").str_or("details", ""),
                "First"
            );

            // Discord speaks first while the app has nothing to say: reading
            // must not get in the way of writing.
            discord.write_frame(3, r#"{"n":1}"#);
            let (op, _) = discord.read_frame();
            assert_eq!(op, 4);

            client.set(None);
            let (_, msg) = discord.read_frame();
            assert!(msg.get("args").get("activity").is_null(), "cleared");

            drop(discord);
            wait_for("waiting again", || client.link() == Link::Waiting);
        }
    }
}
