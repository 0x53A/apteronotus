//! Curated songs available to host applications.

/// A curated song and its metadata.
pub struct Song {
    pub name: &'static str,
    pub title: &'static str,
    pub stresses: &'static str,
    pub source: &'static str,
}

/// The library is empty until songs are selected for publication.
pub static SONGS: &[Song] = &[];

/// Find a song by its stable name.
pub fn song(name: &str) -> Option<&'static Song> {
    SONGS.iter().find(|song| song.name == name)
}
