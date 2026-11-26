//! Favorite folders for the SFTP browser, kept in `favorites.toml`.
//!
//! A favorite is a remote path. One without a Host is shared: it shows on every Host, so
//! common places such as `/var/log` are saved once.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::model::{HostId, Unreadable, load_toml, new_id, save_toml};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Favorite {
    pub id: String,
    pub name: String,
    pub path: String,
    /// The Host it belongs to; `None` shows it on every Host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<HostId>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Favorites {
    #[serde(default, rename = "favorite")]
    pub items: Vec<Favorite>,
}

impl Favorites {
    pub const FILE: &'static str = "favorites.toml";

    /// The saved favorites, or none on first run. A file that can't be read is kept aside (see
    /// [`Unreadable`]): saving over it would lose every favorite.
    pub fn load(dir: &Path) -> (Self, Option<Unreadable>) {
        match load_toml(dir, Self::FILE, "your favorite folders", "favorite folders") {
            Ok(favorites) => (favorites.unwrap_or_default(), None),
            Err(unreadable) => (Self::default(), Some(unreadable)),
        }
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        save_toml(dir, Self::FILE, self)
    }

    /// The favorites shown on `host`: its own, then the shared ones, each in saved order. A
    /// shared one for a path the host has its own favorite for is shown once, as the host's.
    pub fn for_host<'a>(&'a self, host: &'a str) -> impl Iterator<Item = &'a Favorite> + 'a {
        let own = move || self.items.iter().filter(move |f| f.host.as_deref() == Some(host));
        let shared = self.items.iter().filter(move |f| f.host.is_none() && !own().any(|o| o.path == f.path));
        own().chain(shared)
    }

    /// Whether `path` is a favorite on `host` (its own or shared).
    pub fn contains(&self, host: &str, path: &str) -> bool {
        self.for_host(host).any(|f| f.path == path)
    }

    /// Add `path`, named after its last folder. Does nothing if it is already there for the
    /// same scope. Returns the favorite's id.
    pub fn add(&mut self, path: &str, host: Option<&str>) -> String {
        if let Some(existing) = self.items.iter().find(|f| f.path == path && f.host.as_deref() == host) {
            return existing.id.clone();
        }
        let name = crate::sftp::file_name(path);
        let name = if name.is_empty() { "/".to_string() } else { name.to_string() };
        let id = new_id();
        self.items.push(Favorite { id: id.clone(), name, path: path.to_string(), host: host.map(str::to_string) });
        id
    }

    pub fn remove(&mut self, id: &str) {
        self.items.retain(|f| f.id != id);
    }

    pub fn rename(&mut self, id: &str, name: &str) {
        let name = name.trim();
        if let Some(f) = self.items.iter_mut().find(|f| f.id == id)
            && !name.is_empty()
        {
            f.name = name.to_string();
        }
    }

    /// Share a favorite with every Host, or keep it to `host` only. If that scope already has
    /// a favorite for its path, the two become that one.
    pub fn set_host(&mut self, id: &str, host: Option<&str>) {
        let Some(path) = self.items.iter().find(|f| f.id == id).map(|f| f.path.clone()) else { return };
        if self.items.iter().any(|f| f.id != id && f.path == path && f.host.as_deref() == host) {
            return self.remove(id);
        }
        if let Some(f) = self.items.iter_mut().find(|f| f.id == id) {
            f.host = host.map(str::to_string);
        }
    }

    /// Move a favorite up or down among those shown on `host` with the same scope.
    pub fn shift(&mut self, id: &str, host: &str, up: bool) {
        let Some(scope) = self.items.iter().find(|f| f.id == id).map(|f| f.host.clone()) else { return };
        let visible: Vec<usize> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, f)| f.host == scope && (f.host.is_none() || f.host.as_deref() == Some(host)))
            .map(|(i, _)| i)
            .collect();
        let Some(pos) = visible.iter().position(|&i| self.items[i].id == id) else { return };
        let other = if up { pos.checked_sub(1) } else { Some(pos + 1).filter(|&p| p < visible.len()) };
        if let Some(other) = other {
            self.items.swap(visible[pos], visible[other]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn a_path_shared_and_kept_to_a_host_shows_once_there() {
        let mut favs = Favorites::default();
        favs.add("/var/log", None);
        let own = favs.add("/var/log", Some("web01"));
        // Both kinds of "Add to favorites" are offered for it: it used to show twice.
        let web: Vec<&str> = favs.for_host("web01").map(|f| f.id.as_str()).collect();
        assert_eq!(web, [own.as_str()]);
        assert_eq!(favs.for_host("db01").count(), 1);
        // Shared while a shared one exists: one favorite, not two alike.
        favs.set_host(&own, None);
        assert_eq!(favs.items.len(), 1);
        assert_eq!(favs.for_host("web01").count(), 1);
        // Kept to a host while that host has one: the same.
        let own = favs.add("/var/log", Some("web01"));
        let shared = favs.items.iter().find(|f| f.host.is_none()).unwrap().id.clone();
        favs.set_host(&shared, Some("web01"));
        assert_eq!(favs.items.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), [own.as_str()]);
    }

    #[test]
    fn shared_favorites_show_on_every_host() {
        let mut favs = Favorites::default();
        favs.add("/var/log", None);
        favs.add("/srv/app", Some("web01"));
        favs.add("/var/log", None);
        assert_eq!(favs.items.len(), 2);
        let web: Vec<&str> = favs.for_host("web01").map(|f| f.path.as_str()).collect();
        assert_eq!(web, ["/srv/app", "/var/log"]);
        let db: Vec<&str> = favs.for_host("db01").map(|f| f.path.as_str()).collect();
        assert_eq!(db, ["/var/log"]);
        assert!(favs.contains("db01", "/var/log"));
        assert!(!favs.contains("db01", "/srv/app"));
        assert_eq!(favs.items[0].name, "log");
        assert_eq!(favs.items[0].host, None);
    }

    #[test]
    fn reorder_rename_and_remove() {
        let mut favs = Favorites::default();
        let a = favs.add("/a", None);
        let other = favs.add("/x", Some("db01"));
        let b = favs.add("/b", None);
        favs.shift(&b, "web01", true);
        let order: Vec<&str> = favs.for_host("web01").map(|f| f.path.as_str()).collect();
        assert_eq!(order, ["/b", "/a"]);
        // Moving past the end does nothing; other Hosts' favorites are untouched.
        favs.shift(&a, "web01", false);
        assert_eq!(favs.items.iter().position(|f| f.id == other), Some(1));
        favs.rename(&a, "  Alpha ");
        favs.rename(&a, "   ");
        assert_eq!(favs.items.iter().find(|f| f.id == a).unwrap().name, "Alpha");
        favs.set_host(&a, Some("web01"));
        assert!(!favs.contains("db01", "/a"));
        favs.remove(&a);
        assert!(!favs.contains("web01", "/a"));
    }

    #[test]
    fn roundtrips_through_toml() {
        let dir = std::env::temp_dir().join(format!("komando-fav-{}", new_id()));
        let mut favs = Favorites::default();
        favs.add("/etc/nginx", None);
        favs.add("/srv", Some("h1"));
        favs.save(&dir).unwrap();
        assert_eq!(Favorites::load(&dir), (favs, None));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unreadable_favorites_are_kept_aside_rather_than_overwritten() {
        let dir = std::env::temp_dir().join(format!("komando-fav-{}", new_id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(Favorites::FILE), "[[favorite]]\nid = 3").unwrap();
        let (favs, unreadable) = Favorites::load(&dir);
        assert_eq!(favs, Favorites::default());
        let unreadable = unreadable.expect("the user is told");
        assert_eq!(unreadable.in_place, None);
        assert!(!dir.join(Favorites::FILE).exists(), "the next save must not overwrite it");
        let kept: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert!(kept[0].file_name().unwrap().to_str().unwrap().starts_with("favorites.toml.broken-"), "{kept:?}");
        assert!(unreadable.message.contains(&kept[0].display().to_string()), "{}", unreadable.message);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_favorites_are_a_first_run() {
        let dir = std::env::temp_dir().join(format!("komando-fav-{}", new_id()));
        assert_eq!(Favorites::load(&dir), (Favorites::default(), None));
    }
}
