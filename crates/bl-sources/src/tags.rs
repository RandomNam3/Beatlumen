//! Songs of file sources (WebDAV, SFTP): the tags read from the file
//! itself, the file name standing in for a missing title.

use crate::RemoteTrack;

/// A song from a file at `key` (path within the source) read through `src`.
pub fn read(src: &mut dyn bl_media::Source, key: &str, size: u64, modified: u64) -> RemoteTrack {
    let ext = crate::ext_of(key);
    let name = key.rsplit('/').next().unwrap_or(key);
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s).to_string();
    let mut t = RemoteTrack {
        key: key.to_string(),
        title: stem,
        size,
        modified,
        ext: ext.clone(),
        ..Default::default()
    };
    let Ok(info) = bl_media::read_info(src, Some(&ext), false) else {
        return t;
    };
    let g = info.tags;
    if let Some(title) = g.title.filter(|s| !s.trim().is_empty()) {
        t.title = title;
    }
    t.artists = g
        .artist
        .map(|a| {
            a.split(['\0', ';'])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    t.album = g.album.unwrap_or_default();
    t.album_artist = g.album_artist.unwrap_or_default();
    t.track = g.track.unwrap_or(0);
    t.disc = g.disc.unwrap_or(0);
    t.year = g.year.and_then(|y| u32::try_from(y).ok()).unwrap_or(0);
    t.genre = g.genre.unwrap_or_default();
    t.duration_ms = info.duration_ms.unwrap_or(0);
    t
}
