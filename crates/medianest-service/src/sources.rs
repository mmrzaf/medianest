//! Linked folders and scanning.

use std::collections::BTreeMap;
use std::path::Path;

use medianest_catalog::Source;
use medianest_core::{PathBytes, SourceId, SourceKind};

use crate::events::Event;
use crate::jobs::{JobId, Lane};
use crate::{Library, Result};

/// A folder inside a source, with how many media files it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderNode {
    pub source: SourceId,
    /// Path relative to the source root; empty for the root itself.
    pub relpath: PathBytes,
    pub name: String,
    /// Files directly in this folder.
    pub direct: u64,
    /// Files in this folder and all subfolders.
    pub total: u64,
    pub children: Vec<FolderNode>,
}

impl Library {
    pub fn sources(&self) -> Result<Vec<Source>> {
        self.read(medianest_catalog::Db::sources)
    }

    /// Links a folder and starts indexing it.
    pub fn add_folder(&self, folder: &Path) -> Result<(SourceId, JobId)> {
        let source = self.vault().add_source(folder)?;
        self.emit(Event::SourcesChanged);
        self.start_watching();
        let job = self.scan(Some(source), Lane::Foreground);
        Ok((source, job))
    }

    /// Rescans one linked folder, or all of them.
    pub fn scan(&self, source: Option<SourceId>, lane: Lane) -> JobId {
        let title = match source.and_then(|id| self.read(|db| db.source(id)).ok()) {
            Some(source) => format!("Scanning {}", source.name),
            None => "Scanning folders".to_owned(),
        };
        self.submit("scan", title, lane, move |library, context| {
            let sources: Vec<Source> = library
                .sources()?
                .into_iter()
                .filter(|item| item.kind == SourceKind::Linked)
                .filter(|item| source.is_none_or(|id| id == item.id))
                .collect();
            context.set_total(sources.len() as u64);
            let probe = |path: &Path| library.probe(path);
            let (mut added, mut missing, mut moved, mut offline, mut failed) =
                (0, 0, 0, Vec::new(), 0);
            for item in &sources {
                context.checkpoint()?;
                context.set_current(item.name.clone());
                let report = library
                    .vault()
                    .scan_source(item.id, &probe, context.control())?;
                added += report.added;
                missing += report.missing;
                moved += report.moved;
                failed += report.failed.len();
                if report.offline {
                    offline.push(item.name.clone());
                }
                context.advance();
                if report.added > 0 || report.moved > 0 || report.missing > 0 || report.modified > 0
                {
                    library.emit(Event::ContentChanged);
                }
            }
            library.emit(Event::SourcesChanged);
            library.emit(Event::ReviewChanged);
            library.poke_thumbnails();
            library.schedule_hashing();
            let mut parts = vec![match added {
                0 => "No new photos or videos".to_owned(),
                1 => "1 new item".to_owned(),
                n => format!("{n} new items"),
            }];
            if moved > 0 {
                parts.push(format!("{moved} moved"));
            }
            if missing > 0 {
                parts.push(format!("{missing} missing"));
            }
            if failed > 0 {
                parts.push(format!("{failed} unreadable"));
            }
            if !offline.is_empty() {
                parts.push(format!("{} not connected", offline.join(", ")));
            }
            Ok(parts.join(" · "))
        })
    }

    /// Stops tracking a linked folder. Its files are not touched.
    pub fn remove_source(&self, source: SourceId) -> Result<usize> {
        let removed = self.write(|writer| writer.remove_source(source))?;
        for uuid in &removed {
            self.inner.cache.remove(*uuid);
        }
        self.start_watching();
        self.emit(Event::SourcesChanged);
        self.emit(Event::ContentChanged);
        self.emit(Event::ReviewChanged);
        Ok(removed.len())
    }

    pub fn rename_source(&self, source: SourceId, name: &str) -> Result<()> {
        self.write(|writer| writer.rename_source(source, name))?;
        self.emit(Event::SourcesChanged);
        Ok(())
    }

    pub fn set_source_watch(&self, source: SourceId, watch: bool) -> Result<()> {
        self.write(|writer| writer.set_source_watch(source, watch))?;
        self.start_watching();
        self.emit(Event::SourcesChanged);
        Ok(())
    }

    /// Tells MediaNest a linked folder moved, then rescans it there.
    pub fn relocate_source(&self, source: SourceId, folder: &Path) -> Result<JobId> {
        self.vault().relocate_source(source, folder)?;
        self.start_watching();
        self.emit(Event::SourcesChanged);
        Ok(self.scan(Some(source), Lane::Foreground))
    }

    /// The folder tree of a source with file counts, for browsing by folder.
    pub fn folders(&self, source: SourceId) -> Result<FolderNode> {
        let (name, files) =
            self.read(|db| Ok((db.source(source)?.name, db.source_files(source)?)))?;
        // Count files per directory.
        let mut direct: BTreeMap<Vec<u8>, u64> = BTreeMap::new();
        for file in files.iter().filter(|file| file.trash_path.is_none()) {
            let bytes = file.relpath.as_bytes();
            let dir = bytes
                .iter()
                .rposition(|byte| *byte == b'/')
                .map_or(&[][..], |end| &bytes[..end]);
            *direct.entry(dir.to_vec()).or_default() += 1;
            // Make sure every ancestor exists in the map.
            let mut ancestor = dir;
            while let Some(end) = ancestor.iter().rposition(|byte| *byte == b'/') {
                ancestor = &ancestor[..end];
                direct.entry(ancestor.to_vec()).or_default();
            }
            if !dir.is_empty() {
                direct.entry(Vec::new()).or_default();
            }
        }
        fn build(
            source: SourceId,
            relpath: &[u8],
            name: String,
            direct: &BTreeMap<Vec<u8>, u64>,
        ) -> FolderNode {
            let prefix: Vec<u8> = if relpath.is_empty() {
                Vec::new()
            } else {
                [relpath, b"/"].concat()
            };
            let mut children: Vec<FolderNode> = direct
                .keys()
                .filter(|key| {
                    key.starts_with(&prefix)
                        && key.len() > prefix.len()
                        && !key[prefix.len()..].contains(&b'/')
                })
                .map(|key| {
                    let child_name = String::from_utf8_lossy(&key[prefix.len()..]).into_owned();
                    build(source, key, child_name, direct)
                })
                .collect();
            children.sort_by_key(|child| child.name.to_lowercase());
            let own = direct.get(relpath).copied().unwrap_or(0);
            FolderNode {
                source,
                relpath: PathBytes::from_vec(relpath.to_vec()),
                name,
                direct: own,
                total: own + children.iter().map(|child| child.total).sum::<u64>(),
                children,
            }
        }
        Ok(build(source, &[], name, &direct))
    }
}
