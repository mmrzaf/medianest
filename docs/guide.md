# MediaNest user guide

## Your first library

When MediaNest starts for the first time, choose **Create New Library**,
give it a name and pick a disk with plenty of space. MediaNest creates the
folder with a `Media` folder inside, where your photos will live.

To use an existing library, choose **Open Existing Library…** and select
its folder. MediaNest reopens the last library on start; change that in
**Preferences**.

## Importing

Press **Ctrl+I**, click **+** in the sidebar, or drop files and folders onto
the Photos page. Cameras, phones and memory cards that are connected appear at
the top of the import dialog.

MediaNest first looks at what you chose and shows what would happen:

- how many new photos and videos, their size and dates;
- items already in the library — recognised by content, even when renamed —
  which are skipped;
- duplicates within what you chose, imported once;
- items without a reliable date, which go to `Media/Unsorted` and appear in
  **Review**;
- files that are not photos or videos, which are left alone.

Then choose how to import:

- **Copy** keeps the originals where they are. Use this for memory cards you
  will format in the camera, and whenever in doubt.
- **Move** copies and checks each file, then removes its original right
  away, so you never need room for two copies of the whole import. An
  original that changed in the meantime is kept and listed in **Review**.
- **Keep in Place** does not copy anything: the folder is linked and
  watched, and its photos appear in your library while the folder is
  available.

Imports run in the background. Progress shows at the bottom of the sidebar,
where you can pause or stop; stopping is always safe.

## Browsing

**Photos** shows everything by date. Zoom with **Ctrl+plus/minus**,
**Ctrl+scroll** or a pinch; when zoomed out, photos are grouped by month.
Drag the date scrubber on the right edge to jump through the years. Choose
**Square Tiles** in the view menu if you prefer an even grid.

Click a photo to open it. In the viewer, use the arrow keys, swipe, or the
buttons at the sides to move between items. Scroll or pinch to zoom, double
click to switch between fitting the window and 100 %, and drag to pan. The
information panel (**i**) shows the date and where it came from, the camera,
the place on a map, tags, albums and every file location.

Videos play in the viewer. Live Photos and motion photos have a **LIVE**
button that plays their short video.

## Selecting and changing photos

Click the check mark on a photo, press **Ctrl+A**, **Shift**-click a range,
**Ctrl**-click to add or remove, or drag a rectangle across empty space.
Clicking a day's header selects the whole day. The bar at the bottom
offers the common actions; right-click (or press **Menu**) for all of them:

| Action | Key |
|---|---|
| Favorite | F |
| Rating | 0–5 |
| Tags | T |
| Add to album | — |
| Change date | Ctrl+D |
| Rotate | Ctrl+R, Ctrl+Shift+R |
| Hide | Ctrl+H |
| Copy files | Ctrl+C |
| Export | Ctrl+E |
| Move to trash | Delete |

Most changes can be undone from the message that appears, or with
**Ctrl+Z**.

**Change Date** can set an exact date and time, *shift* many items by the
same amount (for a camera whose clock was wrong or set to another time
zone), or go back to the date read from each file.

**Hidden** items disappear from the timeline but are not deleted; search for
`is:hidden` to see them.

## Albums and tags

An **album** holds the photos you put in it, in an order you can choose.
A **smart album** is a saved search, such as `tag:family place:Isfahan`, and
fills itself as you add photos. Create either from **Albums** with **+**,
or select photos and choose **Add to Album**.

**Tags** are words you attach to photos. Rename, merge and delete them on
the **Tags** page.

## Searching

Press **Ctrl+F**, or just start typing on the Photos page. Suggestions for
matching tags, places, albums and cameras appear as you type.

| Search | Finds |
|---|---|
| `beach` | the word in names, tags, places, cameras, albums, captions |
| `tag:family` | items with a tag |
| `album:"Summer 2023"` | items in an album |
| `place:Paris` | items taken in a place |
| `camera:pixel` | items from a camera |
| `2023`, `july`, `july 2023`, `2023-07-14` | items taken then |
| `is:fav`, `is:video`, `is:live`, `is:raw` | favorites, videos, Live Photos, RAW |
| `rating>=4` | rated four stars or more |
| `has:location`, `has:tags` | with a location, with tags |
| `date:unknown`, `date:uncertain` | without a date, with a guessed date |
| `is:hidden` | hidden items |
| `-tag:work` | a leading minus excludes |

All terms must match.

## Places

**Places** shows a map with photos grouped by location; click a group to
zoom in, and again to see the photos taken there. The list beside the map
names countries and cities. Place names are looked up offline; the map
images come from OpenStreetMap and can be turned off in **Preferences**.

## Folders

**Folders** lists the library's own folder and linked folders, which you can
browse like a file manager. For a linked folder you can look for changes,
turn watching on or off, rename it, tell MediaNest where it moved, or remove
it from the library (its files are not touched).

## Review

**Review** collects what needs a decision:

- **Needs a date** — items without a reliable date. Open the list, select
  items and press **Ctrl+D**.
- **Missing files** — files moved or deleted outside MediaNest, or on a disk
  that is not connected. Reconnect, look again, or remove them from the
  library.
- **Changed files** — content that differs from what was imported. Use the
  new version if you edited the file on purpose; otherwise restore it from a
  backup.
- **Originals waiting for removal** — originals a move could not remove
  safely; remove each one or keep it.
- **Interrupted operations** and **failed imports** — check again or retry.

**Similar Photos** finds bursts and near-duplicates and suggests which to
keep.

## Library health

**Library Health** shows whether everything is safe: what the library holds,
when files were last verified, the catalog check, backups and the thumbnail
cache. **Verify Now** re-reads every file; **Back Up Now** writes a catalog
backup; **Restore…** brings back an earlier catalog.

## Trash

Deleting moves files to the system trash. **Trash** lists them; restore
them from there. Items are forgotten when the system trash is emptied, or
when you choose **Remove From Library**. For linked folders you can choose
to remove an item from the library without touching its file.

## Exporting

**Export** copies originals, unchanged, into a folder, optionally sorted into
year and month folders. Files with the same name are never overwritten.
`medianest-cli export metadata` writes tags, albums, ratings, captions and
dates as JSON, and `medianest-cli import-metadata` applies them to another
library.

## Keyboard shortcuts

Press **Ctrl+?** to see every shortcut.
