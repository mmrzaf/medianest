//! `medianest-cli`: every MediaNest operation from the command line.

#![allow(clippy::print_stdout, clippy::print_stderr)]

mod parse;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use medianest_service::catalog::{Scope, Sort, View};
use medianest_service::core::query::Query;
use medianest_service::core::{
    AlbumId, AssetId, CaptureTime, Confidence, DateSource, ReviewId, ReviewKind, SourceId,
    SourceKind, naive_from_local_ms,
};
use medianest_service::vault::{Control, ImportMode, LinkedPolicy};
use medianest_service::{
    ExportOptions, ImportOptions, JobId, JobState, Lane, Library, ReviewAction, Settings,
    group_digits,
};
use serde::Serialize;

#[derive(Parser)]
#[command(
    name = "medianest-cli",
    version,
    about = "Manage a MediaNest photo and video library"
)]
struct Cli {
    /// The library folder. Defaults to $MEDIANEST_LIBRARY, then the current folder.
    #[arg(long, short = 'L', global = true, value_name = "FOLDER")]
    library: Option<PathBuf>,
    /// Print machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new library in FOLDER.
    Init { folder: PathBuf },
    /// Summarize the library.
    Info,
    /// Show the library's health in detail.
    Health,
    /// Manage linked folders.
    #[command(subcommand)]
    Sources(SourcesCommand),
    /// Rescan linked folders for new, changed and missing files.
    Scan { source: Option<i64> },
    /// Copy (or move) photos and videos into the library.
    Import(ImportArgs),
    /// Re-read files and check that their content is unchanged.
    Verify {
        #[arg(long, conflicts_with = "linked")]
        managed: bool,
        #[arg(long)]
        linked: bool,
    },
    /// List assets matching a search, such as `tag:trip 2023 is:fav`.
    Search {
        query: Vec<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long, value_enum, default_value_t = SortArg::Newest)]
        sort: SortArg,
        /// Search the trash instead of the library.
        #[arg(long)]
        trash: bool,
    },
    /// Show everything known about one asset.
    Show { asset: i64 },
    /// Manage tags.
    #[command(subcommand)]
    Tag(TagCommand),
    /// Manage albums.
    #[command(subcommand)]
    Album(AlbumCommand),
    /// Change favorites, ratings, dates, rotation, captions or visibility.
    #[command(subcommand)]
    Set(SetCommand),
    /// Move assets to the desktop trash.
    Trash {
        assets: Vec<i64>,
        /// For files in linked folders, only remove them from the library.
        #[arg(long)]
        keep_linked_files: bool,
    },
    /// Bring assets back from the trash.
    Restore { assets: Vec<i64> },
    /// List and resolve items that need attention.
    #[command(subcommand)]
    Review(ReviewCommand),
    /// Back up or restore the catalog.
    #[command(subcommand)]
    Backup(BackupCommand),
    /// Copy originals or metadata out of the library.
    #[command(subcommand)]
    Export(ExportCommand),
    /// Apply a metadata export to this library.
    ImportMetadata {
        file: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },
    /// List groups of similar-looking photos.
    Similar,
    /// Generate missing thumbnails, or rebuild them.
    Thumbnails {
        #[arg(long, conflicts_with = "failed")]
        rebuild: bool,
        #[arg(long)]
        failed: bool,
    },
}

#[derive(Subcommand)]
enum SourcesCommand {
    List,
    /// Link a folder and index it.
    Add {
        folder: PathBuf,
    },
    /// Stop tracking a linked folder. Its files are not touched.
    Remove {
        source: i64,
    },
    Rename {
        source: i64,
        name: String,
    },
    /// Tell MediaNest a linked folder moved.
    Relocate {
        source: i64,
        folder: PathBuf,
    },
    /// Turn watching for changes on or off.
    Watch {
        source: i64,
        state: OnOff,
    },
    /// Show a linked folder's subfolders with file counts.
    Folders {
        source: i64,
    },
}

#[derive(Args)]
struct ImportArgs {
    /// Files or folders to import.
    #[arg(required = true)]
    paths: Vec<PathBuf>,
    /// Remove each original right after its copy is verified (to free space).
    #[arg(long = "move")]
    move_originals: bool,
    /// Show what would happen without copying anything.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Subcommand)]
enum TagCommand {
    List,
    Add {
        tag: String,
        assets: Vec<i64>,
    },
    Remove {
        tag: String,
        assets: Vec<i64>,
    },
    Rename {
        tag: String,
        name: String,
    },
    Merge {
        from: String,
        into: String,
    },
    /// Delete a tag from every asset. The assets stay.
    Delete {
        tag: String,
    },
}

#[derive(Subcommand)]
enum AlbumCommand {
    List,
    /// Create an album; with --query it is a smart album.
    Create {
        name: String,
        #[arg(long)]
        query: Option<String>,
    },
    Add {
        album: i64,
        assets: Vec<i64>,
    },
    Remove {
        album: i64,
        assets: Vec<i64>,
    },
    Rename {
        album: i64,
        name: String,
    },
    /// Delete an album. Its photos are not affected.
    Delete {
        album: i64,
    },
    Show {
        album: i64,
    },
}

#[derive(Subcommand)]
enum SetCommand {
    Favorite {
        state: OnOff,
        assets: Vec<i64>,
    },
    Rating {
        #[arg(value_parser = clap::value_parser!(u8).range(0..=5))]
        stars: u8,
        assets: Vec<i64>,
    },
    Hidden {
        state: OnOff,
        assets: Vec<i64>,
    },
    /// Set the capture date: YYYY-MM-DD or "YYYY-MM-DD HH:MM".
    Date {
        date: String,
        assets: Vec<i64>,
    },
    /// Shift capture dates, e.g. +1h, -30m, +2d.
    #[command(allow_negative_numbers = true)]
    Shift {
        #[arg(allow_hyphen_values = true)]
        by: String,
        assets: Vec<i64>,
    },
    /// Go back to the date read from the file.
    ResetDate {
        assets: Vec<i64>,
    },
    /// Rotate clockwise by 90, 180 or 270 degrees (-90 for counter-clockwise).
    Rotate {
        #[arg(allow_hyphen_values = true)]
        degrees: i32,
        assets: Vec<i64>,
    },
    Caption {
        asset: i64,
        text: Vec<String>,
    },
}

#[derive(Subcommand)]
enum ReviewCommand {
    /// List open items.
    List {
        #[arg(long, value_enum)]
        kind: Option<KindArg>,
    },
    /// Resolve one item.
    Resolve { item: i64, action: ActionArg },
    /// Resolve every item of a kind.
    ResolveAll { kind: KindArg, action: ActionArg },
}

#[derive(Subcommand)]
enum BackupCommand {
    Create,
    List,
    /// Replace the catalog with a backup (the library must not be open elsewhere).
    Restore {
        file: PathBuf,
    },
}

#[derive(Subcommand)]
enum ExportCommand {
    /// Copy originals of assets into a folder.
    Files {
        destination: PathBuf,
        assets: Vec<i64>,
        #[arg(long)]
        by_date: bool,
    },
    /// Write tags, albums, ratings and dates to a JSON file.
    Metadata { file: PathBuf },
}

#[derive(Clone, Copy, ValueEnum)]
enum OnOff {
    On,
    Off,
}

impl OnOff {
    fn value(self) -> bool {
        matches!(self, Self::On)
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum SortArg {
    Newest,
    Oldest,
    Added,
}

#[derive(Clone, Copy, ValueEnum)]
enum KindArg {
    Missing,
    Changed,
    FailedImport,
    PendingRemoval,
    Interrupted,
    Unreadable,
}

impl KindArg {
    fn kind(self) -> ReviewKind {
        match self {
            Self::Missing => ReviewKind::MissingFile,
            Self::Changed => ReviewKind::ChangedFile,
            Self::FailedImport => ReviewKind::FailedImport,
            Self::PendingRemoval => ReviewKind::PendingRemoval,
            Self::Interrupted => ReviewKind::InterruptedOperation,
            Self::Unreadable => ReviewKind::Unreadable,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum ActionArg {
    Primary,
    Secondary,
    Dismiss,
}

impl ActionArg {
    fn action(self) -> ReviewAction {
        match self {
            Self::Primary => ReviewAction::Primary,
            Self::Secondary => ReviewAction::Secondary,
            Self::Dismiss => ReviewAction::Dismiss,
        }
    }
}

type Outcome = Result<(), String>;

fn ids(values: &[i64]) -> Vec<AssetId> {
    values.iter().copied().map(AssetId).collect()
}

fn require(values: &[i64]) -> Result<Vec<AssetId>, String> {
    if values.is_empty() {
        return Err("give at least one asset id (see `medianest-cli search`)".into());
    }
    Ok(ids(values))
}

fn library_folder(cli: &Cli) -> PathBuf {
    cli.library
        .clone()
        .or_else(|| std::env::var_os("MEDIANEST_LIBRARY").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn settings() -> Settings {
    Settings {
        watch_folders: false,
        ..Settings::default()
    }
}

fn print_json(value: &impl Serialize) -> Outcome {
    let text = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    println!("{text}");
    Ok(())
}

fn date_text(local_ms: Option<i64>) -> String {
    local_ms.and_then(naive_from_local_ms).map_or_else(
        || "no date".to_owned(),
        |date| date.format("%Y-%m-%d %H:%M").to_string(),
    )
}

fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    #[allow(clippy::cast_precision_loss)]
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Waits for a job, drawing a progress line on a terminal.
fn follow(library: &Library, job: JobId, json: bool) -> Outcome {
    let interactive = std::io::stderr().is_terminal() && !json;
    let events = library.subscribe();
    loop {
        let Some(snapshot) = library.job(job) else {
            return Err("the job disappeared".into());
        };
        if snapshot.state.is_finished() {
            if interactive {
                eprint!("\r\x1b[2K");
            }
            if json {
                return print_json(&snapshot);
            }
            return match snapshot.state {
                JobState::Completed => {
                    println!("{}", snapshot.summary);
                    Ok(())
                }
                JobState::Cancelled => Err("stopped".into()),
                _ => Err(snapshot.error.unwrap_or(snapshot.summary)),
            };
        }
        if interactive {
            let percent = snapshot
                .fraction()
                .map(|f| format!(" {:>3.0}%", f * 100.0))
                .unwrap_or_default();
            let count = if snapshot.total > 0 {
                format!(" {}/{}", snapshot.done, snapshot.total)
            } else {
                String::new()
            };
            let mut line = format!("{}{count}{percent} {}", snapshot.title, snapshot.current);
            line.truncate(100);
            eprint!("\r\x1b[2K{line}");
        }
        if events.recv_blocking().is_err() {
            return Err("the library closed".into());
        }
    }
}

fn find_tag(library: &Library, name: &str) -> Result<medianest_service::core::TagId, String> {
    library
        .read(|db| db.tag_by_name(name))
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("no tag named “{name}”"))
}

fn run(cli: &Cli) -> Outcome {
    let json = cli.json;
    if let Command::Init { folder } = &cli.command {
        let library = Library::create(folder, settings()).map_err(|error| error.to_string())?;
        println!("Created a library in {}", library.root().display());
        library.close();
        return Ok(());
    }
    if let Command::Backup(BackupCommand::Restore { file }) = &cli.command {
        let saved = Library::restore_backup(&library_folder(cli), file)
            .map_err(|error| error.to_string())?;
        println!(
            "Catalog restored. The previous catalog was saved as {}",
            saved.display()
        );
        return Ok(());
    }
    let library =
        Library::open(&library_folder(cli), settings()).map_err(|error| error.to_string())?;
    let result = dispatch(&library, &cli.command, json);
    library.close();
    result
}

#[allow(clippy::too_many_lines)]
fn dispatch(library: &Library, command: &Command, json: bool) -> Outcome {
    let fail = |error: medianest_service::Error| error.to_string();
    match command {
        Command::Init { .. } | Command::Backup(BackupCommand::Restore { .. }) => Ok(()),
        Command::Info => {
            let health = library.health().map_err(fail)?;
            if json {
                return print_json(&health);
            }
            let stats = &health.stats;
            println!("Library   {}", library.root().display());
            println!(
                "Items     {} photos · {} videos",
                group_digits(stats.photos),
                group_digits(stats.videos)
            );
            println!(
                "Files     {} in the library ({}) · {} linked ({})",
                group_digits(stats.managed_files),
                size_text(stats.managed_bytes),
                group_digits(stats.linked_files),
                size_text(stats.linked_bytes)
            );
            println!("Status    {}", health.headline());
            Ok(())
        }
        Command::Health => {
            let health = library.health().map_err(fail)?;
            if json {
                return print_json(&health);
            }
            println!("{}", health.headline());
            println!("Catalog check: {}", health.database_message);
            for issue in &health.issues {
                println!("• {} — {}", issue.title, issue.detail);
            }
            let s = &health.stats;
            println!(
                "Files: {} verified, {} not yet checked, {} missing, {} changed",
                s.verified_files, s.unverified_files, s.missing_files, s.changed_files
            );
            println!("Backups: {}", health.backups.len());
            println!(
                "Cache: {} thumbnails, {} previews",
                size_text(health.cache.thumbnails),
                size_text(health.cache.previews)
            );
            if !health.video_tools {
                println!("Video tools (ffmpeg) are not installed: videos have no thumbnails.");
            }
            Ok(())
        }
        Command::Sources(command) => sources(library, command, json),
        Command::Scan { source } => {
            let job = library.scan(source.map(SourceId), Lane::Foreground);
            follow(library, job, json)
        }
        Command::Import(args) => import(library, args, json),
        Command::Verify { managed, linked } => {
            let kind = if *managed {
                Some(SourceKind::Managed)
            } else if *linked {
                Some(SourceKind::Linked)
            } else {
                None
            };
            let job = library.verify(kind, None, Lane::Foreground);
            follow(library, job, json)
        }
        Command::Search {
            query,
            limit,
            sort,
            trash,
        } => {
            let view = View {
                scope: if *trash { Scope::Trash } else { Scope::Library },
                query: Query::parse(&query.join(" ")),
                sort: match sort {
                    SortArg::Newest => Sort::NewestFirst,
                    SortArg::Oldest => Sort::OldestFirst,
                    SortArg::Added => Sort::RecentlyAdded,
                },
            };
            let items = library.read(|db| db.timeline(&view)).map_err(fail)?;
            let shown: Vec<_> = items.iter().take(*limit).collect();
            let assets = library
                .read(|db| db.assets(&shown.iter().map(|item| item.id).collect::<Vec<_>>()))
                .map_err(fail)?;
            if json {
                return print_json(&assets);
            }
            for asset in &assets {
                let star = if asset.favorite { "★" } else { " " };
                println!(
                    "{:>7}  {star} {}  {}",
                    asset.id,
                    date_text(asset.taken.local_ms),
                    asset.name
                );
            }
            if items.len() > shown.len() {
                println!("… {} more (use --limit)", items.len() - shown.len());
            } else if items.is_empty() {
                println!("Nothing found.");
            }
            Ok(())
        }
        Command::Show { asset } => {
            let detail = library.asset_detail(AssetId(*asset)).map_err(fail)?;
            if json {
                return print_json(&serde_json::json!({
                    "asset": detail.asset,
                    "files": detail.files,
                    "tags": detail.tags,
                    "albums": detail.albums,
                }));
            }
            let a = &detail.asset;
            println!("{}  (#{})", a.name, a.id);
            println!(
                "Taken     {} — {}",
                date_text(a.taken.local_ms),
                a.taken.source.describe()
            );
            if let Some((w, h)) = a.display_size() {
                println!(
                    "Size      {w} × {h} · {} · {}",
                    size_text(a.size),
                    a.format.label()
                );
            }
            if let Some(camera) = a.camera_name() {
                println!("Camera    {camera}");
            }
            if let Some(place) = &a.place {
                println!("Place     {}", place.short());
            }
            if !detail.tags.is_empty() {
                println!(
                    "Tags      {}",
                    detail
                        .tags
                        .iter()
                        .map(|t| t.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if !detail.albums.is_empty() {
                println!(
                    "Albums    {}",
                    detail
                        .albums
                        .iter()
                        .map(|al| al.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            println!(
                "Rating    {}{}",
                "★".repeat(usize::from(a.rating)),
                if a.favorite { "  (favorite)" } else { "" }
            );
            for location in &detail.files {
                println!(
                    "File      {} [{:?}]",
                    location.path().display(),
                    location.file.status
                );
            }
            Ok(())
        }
        Command::Tag(command) => tags(library, command, json),
        Command::Album(command) => albums(library, command, json),
        Command::Set(command) => set(library, command),
        Command::Trash {
            assets,
            keep_linked_files,
        } => {
            let policy = if *keep_linked_files {
                LinkedPolicy::RemoveFromLibrary
            } else {
                LinkedPolicy::MoveToTrash
            };
            let report = library.trash(&require(assets)?, policy).map_err(fail)?;
            if json {
                return print_json(&report);
            }
            println!("Moved {} to the trash", report.trashed);
            for (path, reason) in &report.failed {
                eprintln!("Could not trash {path}: {reason}");
            }
            Ok(())
        }
        Command::Restore { assets } => {
            let report = library.restore(&require(assets)?).map_err(fail)?;
            if json {
                return print_json(&report);
            }
            println!("Restored {}", report.restored);
            for (path, reason) in &report.failed {
                eprintln!("Could not restore {path}: {reason}");
            }
            Ok(())
        }
        Command::Review(command) => review(library, command, json),
        Command::Backup(BackupCommand::Create) => {
            let path = library.backup_now().map_err(fail)?;
            println!("Backed up the catalog to {}", path.display());
            Ok(())
        }
        Command::Backup(BackupCommand::List) => {
            let backups = library.backups().map_err(fail)?;
            if json {
                return print_json(&backups);
            }
            for backup in backups {
                let when = chrono::DateTime::from_timestamp_millis(backup.created_ms)
                    .map(|t| {
                        t.with_timezone(&chrono::Local)
                            .format("%Y-%m-%d %H:%M")
                            .to_string()
                    })
                    .unwrap_or_default();
                println!(
                    "{when}  {:>9}  {}",
                    size_text(backup.size),
                    backup.path.display()
                );
            }
            Ok(())
        }
        Command::Export(ExportCommand::Files {
            destination,
            assets,
            by_date,
        }) => {
            let job = library.export_files(
                require(assets)?,
                ExportOptions {
                    destination: destination.clone(),
                    folders_by_date: *by_date,
                    include_motion: true,
                },
            );
            follow(library, job, json)
        }
        Command::Export(ExportCommand::Metadata { file }) => {
            let count = library.export_metadata(file).map_err(fail)?;
            println!("Wrote metadata for {count} items to {}", file.display());
            Ok(())
        }
        Command::ImportMetadata { file, dry_run } => {
            let report = library.import_metadata(file, *dry_run).map_err(fail)?;
            if json {
                return print_json(&report);
            }
            println!(
                "{} of {} entries match this library{}",
                report.matched,
                report.entries,
                if report.applied {
                    "; applied"
                } else {
                    " (dry run, nothing changed)"
                }
            );
            Ok(())
        }
        Command::Similar => {
            let groups = library.similar_groups().map_err(fail)?;
            if json {
                return print_json(&groups.iter().map(|g| g.assets.clone()).collect::<Vec<_>>());
            }
            if groups.is_empty() {
                println!("No similar photos found. Thumbnails must exist first.");
            }
            for group in groups {
                let names = library.read(|db| db.assets(&group.assets)).map_err(fail)?;
                println!(
                    "{}",
                    names
                        .iter()
                        .map(|a| format!("#{} {}", a.id, a.name))
                        .collect::<Vec<_>>()
                        .join("  ≈  ")
                );
            }
            Ok(())
        }
        Command::Thumbnails { rebuild, failed } => {
            if *rebuild || *failed {
                library.rebuild_thumbnails(*failed).map_err(fail)?;
            }
            let events = library.subscribe();
            loop {
                let pending = library
                    .read(|db| Ok(db.stats()?.thumbs_pending))
                    .map_err(fail)?;
                if pending == 0 {
                    break;
                }
                if std::io::stderr().is_terminal() {
                    eprint!("\r\x1b[2K{pending} thumbnails to go");
                }
                let _ = events.recv_blocking();
            }
            eprint!("\r\x1b[2K");
            println!("Thumbnails are up to date");
            Ok(())
        }
    }
}

fn sources(library: &Library, command: &SourcesCommand, json: bool) -> Outcome {
    let fail = |error: medianest_service::Error| error.to_string();
    match command {
        SourcesCommand::List => {
            let sources = library.sources().map_err(fail)?;
            if json {
                return print_json(&sources);
            }
            for source in sources {
                let kind = match source.kind {
                    SourceKind::Managed => "library",
                    SourceKind::Linked => "linked ",
                };
                println!(
                    "{:>3}  {kind}  {:>7} files  {:>9}  {}{}",
                    source.id,
                    group_digits(source.file_count),
                    size_text(source.total_bytes),
                    source.path.display(),
                    if source.status == medianest_service::core::SourceStatus::Offline {
                        "  (not connected)"
                    } else {
                        ""
                    }
                );
            }
            Ok(())
        }
        SourcesCommand::Add { folder } => {
            let (_, job) = library.add_folder(folder).map_err(fail)?;
            follow(library, job, json)
        }
        SourcesCommand::Remove { source } => {
            let removed = library.remove_source(SourceId(*source)).map_err(fail)?;
            println!(
                "Stopped tracking the folder; {removed} items left the library. No files were touched."
            );
            Ok(())
        }
        SourcesCommand::Rename { source, name } => {
            library.rename_source(SourceId(*source), name).map_err(fail)
        }
        SourcesCommand::Relocate { source, folder } => {
            let job = library
                .relocate_source(SourceId(*source), folder)
                .map_err(fail)?;
            follow(library, job, json)
        }
        SourcesCommand::Watch { source, state } => library
            .set_source_watch(SourceId(*source), state.value())
            .map_err(fail),
        SourcesCommand::Folders { source } => {
            let tree = library.folders(SourceId(*source)).map_err(fail)?;
            fn print(node: &medianest_service::FolderNode, depth: usize) {
                println!(
                    "{}{} ({})",
                    "  ".repeat(depth),
                    if depth == 0 { "/" } else { &node.name },
                    node.total
                );
                for child in &node.children {
                    print(child, depth + 1);
                }
            }
            print(&tree, 0);
            Ok(())
        }
    }
}

fn import(library: &Library, args: &ImportArgs, json: bool) -> Outcome {
    let interactive = std::io::stderr().is_terminal() && !json;
    let progress = move |done: u64, total: u64| {
        if interactive {
            eprint!("\r\x1b[2KExamining {done}/{total}");
        }
    };
    let plan = library
        .plan_import(&args.paths, &Control::new(), &progress)
        .map_err(|error| error.to_string())?;
    if interactive {
        eprint!("\r\x1b[2K");
    }
    let mode = if args.move_originals {
        ImportMode::Move
    } else {
        ImportMode::Copy
    };
    let space = library
        .space_check(&plan, mode)
        .map_err(|error| error.to_string())?;
    if args.dry_run || json {
        if json {
            return print_json(&serde_json::json!({
                "new": plan.new_count,
                "new_bytes": plan.new_bytes,
                "photos": plan.photos,
                "videos": plan.videos,
                "already_in_library": plan.already_in_library,
                "duplicates_in_selection": plan.duplicates_in_selection,
                "unreadable": plan.unreadable,
                "other_files": plan.other_files,
                "uncertain_dates": plan.uncertain_dates,
                "months": plan.months,
                "space_required": space.required,
                "space_available": space.available,
            }));
        }
        println!(
            "{} new items ({})",
            plan.new_count,
            size_text(plan.new_bytes)
        );
        println!("  {} photos, {} videos", plan.photos, plan.videos);
        println!(
            "  needs {} free, {} available",
            size_text(space.required),
            size_text(space.available)
        );
        if plan.already_in_library > 0 {
            println!("  {} already in the library", plan.already_in_library);
        }
        if plan.duplicates_in_selection > 0 {
            println!(
                "  {} duplicates within the selection",
                plan.duplicates_in_selection
            );
        }
        if plan.uncertain_dates > 0 {
            println!(
                "  {} with an uncertain date, going to Unsorted",
                plan.uncertain_dates
            );
        }
        for item in plan
            .items
            .iter()
            .filter(|i| matches!(i.state, medianest_service::ImportItemState::Unreadable(_)))
        {
            if let medianest_service::ImportItemState::Unreadable(reason) = &item.state {
                println!("  unreadable: {} ({reason})", item.path.display());
            }
        }
        if args.dry_run {
            println!("Dry run: nothing was copied.");
            return Ok(());
        }
    }
    if plan.new_count == 0 && !args.move_originals {
        println!("Nothing new to import.");
        return Ok(());
    }
    if space.is_short() {
        return Err(format!(
            "not enough free space: {} needed, {} available",
            size_text(space.required),
            size_text(space.available)
        ));
    }
    let job = library.import(plan, ImportOptions { mode });
    follow(library, job, json)
}

fn tags(library: &Library, command: &TagCommand, json: bool) -> Outcome {
    let fail = |error: medianest_service::Error| error.to_string();
    match command {
        TagCommand::List => {
            let tags = library.read(|db| db.tags()).map_err(fail)?;
            if json {
                return print_json(&tags);
            }
            for tag in tags {
                println!("{:>7}  {}", group_digits(tag.count), tag.name);
            }
            Ok(())
        }
        TagCommand::Add { tag, assets } => library
            .add_tag(&require(assets)?, tag)
            .map(|_| ())
            .map_err(fail),
        TagCommand::Remove { tag, assets } => {
            let id = find_tag(library, tag)?;
            library.remove_tag(&require(assets)?, id).map_err(fail)
        }
        TagCommand::Rename { tag, name } => {
            let id = find_tag(library, tag)?;
            library.rename_tag(id, name).map(|_| ()).map_err(fail)
        }
        TagCommand::Merge { from, into } => {
            let (from, into) = (find_tag(library, from)?, find_tag(library, into)?);
            library.merge_tags(from, into).map_err(fail)
        }
        TagCommand::Delete { tag } => {
            let id = find_tag(library, tag)?;
            library.delete_tag(id).map_err(fail)
        }
    }
}

fn albums(library: &Library, command: &AlbumCommand, json: bool) -> Outcome {
    let fail = |error: medianest_service::Error| error.to_string();
    match command {
        AlbumCommand::List => {
            let albums = library.read(|db| db.albums()).map_err(fail)?;
            if json {
                return print_json(&albums);
            }
            for album in albums {
                let count = match &album.query {
                    Some(query) => library
                        .read(|db| db.count(&View::library().with_query(Query::parse(query))))
                        .map_err(fail)?,
                    None => album.count,
                };
                let smart = album
                    .query
                    .as_ref()
                    .map(|q| format!("  (smart: {q})"))
                    .unwrap_or_default();
                println!(
                    "{:>4}  {:>6}  {}{smart}",
                    album.id,
                    group_digits(count),
                    album.name
                );
            }
            Ok(())
        }
        AlbumCommand::Create { name, query } => {
            let id = library.create_album(name, query.as_deref()).map_err(fail)?;
            println!("Created album #{id}");
            Ok(())
        }
        AlbumCommand::Add { album, assets } => {
            let added = library
                .add_to_album(AlbumId(*album), &require(assets)?)
                .map_err(fail)?;
            println!("Added {added}");
            Ok(())
        }
        AlbumCommand::Remove { album, assets } => library
            .remove_from_album(AlbumId(*album), &require(assets)?)
            .map_err(fail),
        AlbumCommand::Rename { album, name } => {
            library.rename_album(AlbumId(*album), name).map_err(fail)
        }
        AlbumCommand::Delete { album } => library.delete_album(AlbumId(*album)).map_err(fail),
        AlbumCommand::Show { album } => {
            let album = library.read(|db| db.album(AlbumId(*album))).map_err(fail)?;
            let view = match &album.query {
                Some(query) => View::library().with_query(Query::parse(query)),
                None => View {
                    scope: Scope::Album(album.id),
                    query: Query::default(),
                    sort: Sort::AlbumOrder,
                },
            };
            let ids = library.read(|db| db.view_ids(&view)).map_err(fail)?;
            let assets = library.read(|db| db.assets(&ids)).map_err(fail)?;
            if json {
                return print_json(&assets);
            }
            for asset in assets {
                println!(
                    "{:>7}  {}  {}",
                    asset.id,
                    date_text(asset.taken.local_ms),
                    asset.name
                );
            }
            Ok(())
        }
    }
}

fn set(library: &Library, command: &SetCommand) -> Outcome {
    let fail = |error: medianest_service::Error| error.to_string();
    match command {
        SetCommand::Favorite { state, assets } => library
            .set_favorite(&require(assets)?, state.value())
            .map_err(fail),
        SetCommand::Rating { stars, assets } => {
            library.set_rating(&require(assets)?, *stars).map_err(fail)
        }
        SetCommand::Hidden { state, assets } => library
            .set_hidden(&require(assets)?, state.value())
            .map_err(fail),
        SetCommand::Date { date, assets } => {
            let local = parse::datetime(date)?;
            library
                .set_taken(
                    &require(assets)?,
                    CaptureTime::floating(local, DateSource::Manual, Confidence::High),
                )
                .map_err(fail)
        }
        SetCommand::Shift { by, assets } => {
            let delta = parse::duration_ms(by)?;
            library.shift_taken(&require(assets)?, delta).map_err(fail)
        }
        SetCommand::ResetDate { assets } => library.reset_taken(&require(assets)?).map_err(fail),
        SetCommand::Rotate { degrees, assets } => {
            library.rotate(&require(assets)?, *degrees).map_err(fail)
        }
        SetCommand::Caption { asset, text } => {
            let text = text.join(" ");
            library
                .set_caption(AssetId(*asset), (!text.is_empty()).then_some(text.as_str()))
                .map_err(fail)
        }
    }
}

fn review(library: &Library, command: &ReviewCommand, json: bool) -> Outcome {
    let fail = |error: medianest_service::Error| error.to_string();
    match command {
        ReviewCommand::List { kind } => {
            let items = library
                .review_items(kind.map(KindArg::kind))
                .map_err(fail)?;
            if json {
                return print_json(&items);
            }
            if items.is_empty() {
                println!("Nothing needs your attention.");
            }
            let mut current = None;
            for item in items {
                if current != Some(item.kind) {
                    println!("{}", item.kind.title());
                    current = Some(item.kind);
                }
                let detail = item
                    .detail
                    .get("reason")
                    .or_else(|| item.detail.get("source"))
                    .or_else(|| item.detail.get("path"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                println!("{:>6}  {}  {detail}", item.id, item.subject);
            }
            Ok(())
        }
        ReviewCommand::Resolve { item, action } => {
            let message = library
                .resolve_review(ReviewId(*item), action.action())
                .map_err(fail)?;
            println!("{message}");
            Ok(())
        }
        ReviewCommand::ResolveAll { kind, action } => {
            let (done, errors) = library
                .resolve_all(kind.kind(), action.action())
                .map_err(fail)?;
            println!("Resolved {done}");
            for error in &errors {
                eprintln!("{error}");
            }
            if errors.is_empty() {
                Ok(())
            } else {
                Err(format!("{} could not be resolved", errors.len()))
            }
        }
    }
}

fn main() -> ExitCode {
    medianest_service::media::run_helper_if_requested();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("MEDIANEST_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("medianest-cli: {message}");
            ExitCode::FAILURE
        }
    }
}
