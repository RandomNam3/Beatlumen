//! Favorite songs of a login, newest first: one database entry per song
//! (`<scope>favorite/<id>`: when it was marked). Like playlists they stay
//! when a song is gone from the library for a while.

use crate::store::Part;
use crate::util;
use bl_json::Value;
use std::sync::RwLock;

const MAX_ITEMS: usize = 100_000;

pub type Failure = (u16, String);

#[derive(Debug, Clone, PartialEq)]
pub struct Favorite {
    pub id: String,
    /// When it was marked (seconds since the Unix epoch).
    pub added: u64,
}

pub struct Favorites {
    part: Part,
    list: RwLock<Vec<Favorite>>,
}

fn failed(e: crate::store::Error) -> Failure {
    crate::error!("cannot save favorites: {e}");
    (500, "cannot save favorites".to_string())
}

impl Favorites {
    pub fn load(part: Part) -> Favorites {
        let mut list: Vec<Favorite> = part
            .list()
            .into_iter()
            .filter(|(id, _)| util::is_id(id))
            .map(|(id, t)| Favorite {
                id,
                added: std::str::from_utf8(&t)
                    .ok()
                    .and_then(|t| t.parse().ok())
                    .unwrap_or(0),
            })
            .collect();
        list.sort_by_key(|f| std::cmp::Reverse(f.added));
        Favorites {
            part,
            list: RwLock::new(list),
        }
    }

    fn json(list: &[Favorite]) -> Value {
        Value::object().with(
            "items",
            Value::Array(
                list.iter()
                    .map(|f| Value::object().with("id", f.id.as_str()).with("t", f.added))
                    .collect(),
            ),
        )
    }

    pub fn list_json(&self) -> Value {
        Self::json(&self.list.read().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn ids(&self) -> Vec<Favorite> {
        self.list.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// `PUT /api/favorites/{id}`, optional body `{t}`: when it was marked
    /// (a device that was offline).
    pub fn add(&self, id: &str, body: &[u8]) -> Result<(), Failure> {
        if !util::is_id(id) {
            return Err((400, "invalid song id".into()));
        }
        let now = util::now_secs();
        let added = bl_json::parse_bytes(body)
            .ok()
            .and_then(|v| v.get("t").as_u64())
            .filter(|t| *t <= now + 86_400)
            .unwrap_or(now);
        let mut list = self.list.write().unwrap_or_else(|e| e.into_inner());
        if list.iter().any(|f| f.id == id) {
            return Ok(());
        }
        if list.len() >= MAX_ITEMS {
            return Err((409, "too many favorites".into()));
        }
        self.part
            .put(id, added.to_string().as_bytes())
            .map_err(failed)?;
        let at = list.partition_point(|f| f.added > added);
        list.insert(
            at,
            Favorite {
                id: id.to_string(),
                added,
            },
        );
        Ok(())
    }

    /// `DELETE /api/favorites/{id}`; fine when it is not a favorite.
    pub fn remove(&self, id: &str) -> Result<(), Failure> {
        let mut list = self.list.write().unwrap_or_else(|e| e.into_inner());
        let Some(i) = list.iter().position(|f| f.id == id) else {
            return Ok(());
        };
        self.part.delete(id).map_err(failed)?;
        list.remove(i);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> Part {
        Part::new(
            &crate::store::temp_store(&format!("fav-{name}")),
            "shared/favorite/",
        )
    }

    const A: &str = "00000000000000aa";
    const B: &str = "00000000000000bb";

    #[test]
    fn newest_first_and_kept_over_restart() {
        let p = temp("order");
        let f = Favorites::load(p.clone());
        f.add(A, br#"{"t":100}"#).unwrap();
        f.add(B, br#"{"t":200}"#).unwrap();
        f.add(A, br#"{"t":300}"#).unwrap();
        let ids: Vec<_> = f.ids().into_iter().map(|x| x.id).collect();
        assert_eq!(ids, [B, A]);
        let again = Favorites::load(p);
        assert_eq!(again.ids().len(), 2);
        again.remove(B).unwrap();
        again.remove(B).unwrap();
        assert_eq!(again.ids().len(), 1);
    }

    #[test]
    fn only_song_ids() {
        let f = Favorites::load(temp("ids"));
        assert_eq!(f.add("../secret", b"").unwrap_err().0, 400);
        assert_eq!(f.add("file:/etc/passwd", b"").unwrap_err().0, 400);
        assert!(f.ids().is_empty());
    }

    #[test]
    fn a_time_from_the_future_is_not_taken() {
        let f = Favorites::load(temp("future"));
        f.add(A, br#"{"t":99999999999}"#).unwrap();
        assert!(f.ids()[0].added <= util::now_secs());
    }
}
