//! Changes people make: favorites, ratings, tags, albums, dates and more.

use medianest_catalog::{Album, Asset, FileLocation, Tag};
use medianest_core::{AlbumId, AlbumSort, AssetId, CaptureTime, TagId};
use medianest_vault::{LinkedPolicy, TrashReport};

use crate::events::Event;
use crate::{Library, Result};

/// Everything the info panel shows about one asset.
#[derive(Clone, Debug, PartialEq)]
pub struct AssetDetail {
    pub asset: Asset,
    pub files: Vec<FileLocation>,
    pub tags: Vec<Tag>,
    pub albums: Vec<Album>,
}

impl Library {
    pub fn asset_detail(&self, id: AssetId) -> Result<AssetDetail> {
        self.read(|db| {
            Ok(AssetDetail {
                asset: db.asset(id)?,
                files: db.files_of(id)?,
                tags: db.tags_of(id)?,
                albums: db.albums_of(id)?,
            })
        })
    }

    pub fn set_favorite(&self, ids: &[AssetId], favorite: bool) -> Result<()> {
        self.write(|writer| writer.set_favorite(ids, favorite))?;
        self.emit(Event::AssetsChanged(ids.to_vec()));
        Ok(())
    }

    pub fn set_rating(&self, ids: &[AssetId], rating: u8) -> Result<()> {
        self.write(|writer| writer.set_rating(ids, rating))?;
        self.emit(Event::AssetsChanged(ids.to_vec()));
        Ok(())
    }

    /// Hidden assets only appear when searching for `is:hidden`.
    pub fn set_hidden(&self, ids: &[AssetId], hidden: bool) -> Result<()> {
        self.write(|writer| writer.set_hidden(ids, hidden))?;
        self.emit(Event::ContentChanged);
        Ok(())
    }

    pub fn set_caption(&self, id: AssetId, caption: Option<&str>) -> Result<()> {
        self.write(|writer| writer.set_caption(id, caption))?;
        self.emit(Event::AssetsChanged(vec![id]));
        Ok(())
    }

    /// Rotates clockwise by a multiple of 90 degrees. Files are not modified.
    pub fn rotate(&self, ids: &[AssetId], degrees: i32) -> Result<()> {
        self.write(|writer| writer.rotate(ids, degrees))?;
        self.emit(Event::AssetsChanged(ids.to_vec()));
        Ok(())
    }

    /// Sets the capture date and time of assets.
    pub fn set_taken(&self, ids: &[AssetId], time: CaptureTime) -> Result<()> {
        self.write(|writer| writer.set_taken(ids, time))?;
        self.emit(Event::ContentChanged);
        Ok(())
    }

    /// Moves capture times by a fixed amount, for a camera whose clock was wrong.
    pub fn shift_taken(&self, ids: &[AssetId], delta_ms: i64) -> Result<()> {
        self.write(|writer| writer.shift_taken(ids, delta_ms))?;
        self.emit(Event::ContentChanged);
        Ok(())
    }

    /// Goes back to the date read from the file.
    pub fn reset_taken(&self, ids: &[AssetId]) -> Result<()> {
        self.write(|writer| writer.reset_taken(ids))?;
        self.emit(Event::ContentChanged);
        Ok(())
    }

    // ----- tags ----------------------------------------------------------------

    pub fn add_tag(&self, ids: &[AssetId], name: &str) -> Result<TagId> {
        let tag = self.write(|writer| writer.add_tag(ids, name))?;
        self.emit(Event::AssetsChanged(ids.to_vec()));
        self.emit(Event::TagsChanged);
        Ok(tag)
    }

    pub fn remove_tag(&self, ids: &[AssetId], tag: TagId) -> Result<()> {
        self.write(|writer| writer.remove_tag(ids, tag))?;
        self.emit(Event::AssetsChanged(ids.to_vec()));
        self.emit(Event::TagsChanged);
        Ok(())
    }

    /// Renames a tag; an existing tag with the new name absorbs it.
    pub fn rename_tag(&self, tag: TagId, name: &str) -> Result<TagId> {
        let result = self.write(|writer| writer.rename_tag(tag, name))?;
        self.emit(Event::TagsChanged);
        self.emit(Event::ContentChanged);
        Ok(result)
    }

    pub fn merge_tags(&self, from: TagId, into: TagId) -> Result<()> {
        self.write(|writer| writer.merge_tags(from, into))?;
        self.emit(Event::TagsChanged);
        self.emit(Event::ContentChanged);
        Ok(())
    }

    /// Removes a tag from every photo. The photos themselves stay.
    pub fn delete_tag(&self, tag: TagId) -> Result<()> {
        self.write(|writer| writer.delete_tag(tag))?;
        self.emit(Event::TagsChanged);
        self.emit(Event::ContentChanged);
        Ok(())
    }

    // ----- albums --------------------------------------------------------------

    /// Creates a manual album, or a smart album when `query` is given.
    pub fn create_album(&self, name: &str, query: Option<&str>) -> Result<AlbumId> {
        let album = self.write(|writer| writer.create_album(name, query))?;
        self.emit(Event::AlbumsChanged);
        Ok(album)
    }

    pub fn rename_album(&self, album: AlbumId, name: &str) -> Result<()> {
        self.write(|writer| writer.rename_album(album, name))?;
        self.emit(Event::AlbumsChanged);
        Ok(())
    }

    pub fn set_album_query(&self, album: AlbumId, query: &str) -> Result<()> {
        self.write(|writer| writer.set_album_query(album, query))?;
        self.emit(Event::AlbumsChanged);
        Ok(())
    }

    pub fn set_album_sort(&self, album: AlbumId, sort: AlbumSort) -> Result<()> {
        self.write(|writer| writer.set_album_sort(album, sort))?;
        self.emit(Event::AlbumsChanged);
        Ok(())
    }

    pub fn set_album_cover(&self, album: AlbumId, cover: Option<AssetId>) -> Result<()> {
        self.write(|writer| writer.set_album_cover(album, cover))?;
        self.emit(Event::AlbumsChanged);
        Ok(())
    }

    /// Deletes an album. Its photos are not affected.
    pub fn delete_album(&self, album: AlbumId) -> Result<()> {
        self.write(|writer| writer.delete_album(album))?;
        self.emit(Event::AlbumsChanged);
        Ok(())
    }

    pub fn add_to_album(&self, album: AlbumId, ids: &[AssetId]) -> Result<usize> {
        let added = self.write(|writer| writer.add_to_album(album, ids))?;
        self.emit(Event::AlbumsChanged);
        self.emit(Event::AssetsChanged(ids.to_vec()));
        Ok(added)
    }

    pub fn remove_from_album(&self, album: AlbumId, ids: &[AssetId]) -> Result<()> {
        self.write(|writer| writer.remove_from_album(album, ids))?;
        self.emit(Event::AlbumsChanged);
        self.emit(Event::ContentChanged);
        Ok(())
    }

    pub fn reorder_album(
        &self,
        album: AlbumId,
        ids: &[AssetId],
        before: Option<AssetId>,
    ) -> Result<()> {
        self.write(|writer| writer.reorder_album(album, ids, before))?;
        self.emit(Event::ContentChanged);
        Ok(())
    }

    /// A cover for an album: its chosen cover, or its first photo.
    pub fn album_cover(&self, album: &Album) -> Result<Option<AssetId>> {
        if let Some(cover) = album.cover {
            return Ok(Some(cover));
        }
        let view = match &album.query {
            Some(query) => medianest_catalog::View::library()
                .with_query(medianest_core::query::Query::parse(query)),
            None => medianest_catalog::View {
                scope: medianest_catalog::Scope::Album(album.id),
                query: medianest_core::query::Query::default(),
                sort: medianest_catalog::Sort::NewestFirst,
            },
        };
        self.read(|db| db.first_of(&view))
    }

    // ----- trash ---------------------------------------------------------------

    /// Moves assets to the desktop trash. Linked files follow `linked`.
    pub fn trash(&self, ids: &[AssetId], linked: LinkedPolicy) -> Result<TrashReport> {
        let report = self.vault().trash_assets(ids, linked)?;
        for uuid in &report.forgotten {
            self.inner.cache.remove(*uuid);
        }
        self.emit(Event::ContentChanged);
        Ok(report)
    }

    /// Brings assets back from the trash.
    pub fn restore(&self, ids: &[AssetId]) -> Result<TrashReport> {
        let report = self.vault().restore_assets(ids)?;
        self.emit(Event::ContentChanged);
        Ok(report)
    }

    /// Removes trashed assets from the library. Their files stay in the
    /// desktop trash until it is emptied.
    pub fn forget_trashed(&self, ids: &[AssetId]) -> Result<usize> {
        let mut forgotten = 0;
        for &id in ids {
            let trashed = self
                .read(|db| db.try_asset(id))?
                .is_some_and(|asset| asset.trashed_ms.is_some());
            if trashed {
                let uuid = self.write(|writer| writer.forget_asset(id))?;
                self.inner.cache.remove(uuid);
                forgotten += 1;
            }
        }
        self.emit(Event::ContentChanged);
        Ok(forgotten)
    }
}
