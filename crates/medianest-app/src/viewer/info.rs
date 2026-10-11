//! Everything known about the photo on screen, and quick edits.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use medianest_service::AssetDetail;
use medianest_service::core::{AssetId, FileStatus, SourceKind};
use shumate::prelude::*;

use crate::actions;
use crate::context::{Ctx, Destination};
use crate::dialogs::tags::TagInput;
use crate::i18n::{tr, trf};
use crate::util;

pub struct InfoPanel {
    ctx: Rc<Ctx>,
    root: adw::ToolbarView,
    page: adw::PreferencesPage,
    caption: adw::EntryRow,
    date_row: adw::ActionRow,
    stars: Vec<gtk::Button>,
    details: adw::PreferencesGroup,
    detail_rows: RefCell<Vec<gtk::Widget>>,
    place: adw::PreferencesGroup,
    place_row: adw::ActionRow,
    map: RefCell<Option<shumate::SimpleMap>>,
    marker: RefCell<Option<shumate::Marker>>,
    tags_box: gtk::FlowBox,
    /// Kept alive for its suggestions.
    _tag_input: Rc<TagInput>,
    albums: adw::PreferencesGroup,
    album_rows: RefCell<Vec<gtk::Widget>>,
    files: adw::PreferencesGroup,
    file_rows: RefCell<Vec<gtk::Widget>>,
    current: Cell<Option<AssetId>>,
    rating: Cell<u8>,
    caption_text: RefCell<String>,
}

impl InfoPanel {
    pub fn new(ctx: Rc<Ctx>) -> Rc<Self> {
        let caption = adw::EntryRow::builder()
            .title(tr("Caption"))
            .show_apply_button(true)
            .build();
        let date_row = adw::ActionRow::builder()
            .title(tr("Date"))
            .activatable(true)
            .tooltip_text(tr("Change Date"))
            .build();
        date_row.add_suffix(&gtk::Image::from_icon_name("document-edit-symbolic"));
        let stars_box = gtk::Box::builder()
            .spacing(2)
            .valign(gtk::Align::Center)
            .build();
        let mut stars = Vec::new();
        for value in 1..=5u8 {
            let star = gtk::Button::builder()
                .icon_name("non-starred-symbolic")
                .css_classes(["flat", "circular", "rating"])
                .tooltip_text(trf("{} Stars", &[&value.to_string()]))
                .build();
            stars_box.append(&star);
            stars.push(star);
        }
        let rating_row = adw::ActionRow::builder().title(tr("Rating")).build();
        rating_row.add_suffix(&stars_box);
        let summary = adw::PreferencesGroup::new();
        summary.add(&caption);
        summary.add(&date_row);
        summary.add(&rating_row);

        let details = adw::PreferencesGroup::builder()
            .title(tr("Details"))
            .build();

        let place_row = adw::ActionRow::builder()
            .css_classes(["property"])
            .title(tr("Place"))
            .build();
        let place = adw::PreferencesGroup::builder()
            .title(tr("Location"))
            .visible(false)
            .build();
        place.add(&place_row);

        let tags_box = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .column_spacing(6)
            .row_spacing(6)
            .max_children_per_line(8)
            .build();
        let tags_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(9)
            .build();
        tags_content.append(&tags_box);
        let tags = adw::PreferencesGroup::builder().title(tr("Tags")).build();
        tags.add(&tags_content);

        let albums = adw::PreferencesGroup::builder()
            .title(tr("Albums"))
            .visible(false)
            .build();
        let files = adw::PreferencesGroup::builder().title(tr("Files")).build();

        let page = adw::PreferencesPage::new();
        page.add(&summary);
        page.add(&details);
        page.add(&place);
        page.add(&tags);
        page.add(&albums);
        page.add(&files);

        let header = adw::HeaderBar::builder()
            .show_back_button(false)
            .show_end_title_buttons(false)
            .show_start_title_buttons(false)
            .title_widget(&adw::WindowTitle::new(&tr("Information"), ""))
            .build();
        let root = adw::ToolbarView::new();
        root.add_top_bar(&header);
        root.set_content(Some(&page));
        root.add_css_class("info-panel");

        let this = Rc::new_cyclic(|weak: &std::rc::Weak<Self>| {
            let weak = weak.clone();
            let tag_input = TagInput::new(&ctx, move |name| {
                if let Some(this) = weak.upgrade() {
                    this.add_tag(name);
                }
            });
            tags_content.append(tag_input.widget());
            Self {
                ctx,
                root,
                page,
                caption,
                date_row,
                stars,
                details,
                detail_rows: RefCell::default(),
                place,
                place_row,
                map: RefCell::default(),
                marker: RefCell::default(),
                tags_box,
                _tag_input: tag_input,
                albums,
                album_rows: RefCell::default(),
                files,
                file_rows: RefCell::default(),
                current: Cell::new(None),
                rating: Cell::new(0),
                caption_text: RefCell::default(),
            }
        });
        this.setup();
        this
    }

    pub fn widget(&self) -> &adw::ToolbarView {
        &self.root
    }

    fn setup(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.caption.connect_apply(move |row| {
            let Some(this) = weak.upgrade() else { return };
            let Some(id) = this.current.get() else { return };
            let text = row.text().trim().to_owned();
            *this.caption_text.borrow_mut() = text.clone();
            let ctx = Rc::clone(&this.ctx);
            util::spawn(async move {
                let caption = (!text.is_empty()).then_some(text);
                if let Err(error) = ctx
                    .call(move |library| library.set_caption(id, caption.as_deref()))
                    .await
                {
                    ctx.error(&tr("Could not save the caption"), &error);
                }
            });
        });
        let weak = Rc::downgrade(self);
        self.date_row.connect_activated(move |_| {
            if let Some(this) = weak.upgrade() {
                if let Some(id) = this.current.get() {
                    crate::dialogs::date::show(&this.ctx, vec![id]);
                }
            }
        });
        for (index, star) in self.stars.iter().enumerate() {
            let weak = Rc::downgrade(self);
            let value = u8::try_from(index + 1).unwrap_or(5);
            star.connect_clicked(move |_| {
                let Some(this) = weak.upgrade() else { return };
                let Some(id) = this.current.get() else { return };
                // Clicking the current rating clears it.
                let rating = if this.rating.get() == value { 0 } else { value };
                this.show_rating(rating);
                actions::set_rating(&this.ctx, vec![id], rating);
            });
        }
    }

    fn add_tag(&self, name: String) {
        let Some(id) = self.current.get() else { return };
        let ctx = Rc::clone(&self.ctx);
        util::spawn(async move {
            if let Err(error) = ctx.call(move |library| library.add_tag(&[id], &name)).await {
                ctx.error(&tr("Could not add the tag"), &error);
            }
        });
    }

    fn show_rating(&self, rating: u8) {
        self.rating.set(rating);
        for (index, star) in self.stars.iter().enumerate() {
            let filled = index < usize::from(rating);
            star.set_icon_name(if filled {
                "starred-symbolic"
            } else {
                "non-starred-symbolic"
            });
            if filled {
                star.add_css_class("filled");
            } else {
                star.remove_css_class("filled");
            }
        }
    }

    pub fn set_error(&self, message: &str) {
        self.date_row
            .set_subtitle(&glib::markup_escape_text(message));
    }

    pub fn set_detail(self: &Rc<Self>, detail: &AssetDetail) {
        let asset = &detail.asset;
        let changed_asset = self.current.replace(Some(asset.id)) != Some(asset.id);
        if changed_asset {
            self.page.scroll_to_top();
        }
        let caption = asset.caption.clone().unwrap_or_default();
        if changed_asset || *self.caption_text.borrow() != caption {
            self.caption.set_text(&caption);
            *self.caption_text.borrow_mut() = caption;
        }

        self.date_row.set_title(&match asset.taken.local_ms {
            Some(ms) => util::date_time(ms),
            None => tr("Unknown date"),
        });
        let mut source = tr(asset.taken.source.describe());
        let offset = asset
            .taken
            .offset_minutes
            .filter(|_| asset.taken.confidence >= medianest_service::core::Confidence::High);
        if let Some(offset) = offset {
            let sign = if offset < 0 { '−' } else { '+' };
            let offset = offset.unsigned_abs();
            source.push_str(&format!(" · UTC{sign}{}:{:02}", offset / 60, offset % 60));
        }
        if !asset.taken.is_trustworthy() {
            source.push_str(" · ");
            source.push_str(&tr("may be wrong"));
        }
        self.date_row
            .set_subtitle(&glib::markup_escape_text(&source));
        self.show_rating(asset.rating);

        // Details.
        for row in self.detail_rows.borrow_mut().drain(..) {
            self.details.remove(&row);
        }
        let mut rows: Vec<(String, String)> = vec![(tr("Name"), asset.name.clone())];
        let mut size = Vec::new();
        if let Some((width, height)) = asset.display_size() {
            size.push(format!("{width} × {height}"));
        }
        if let Some(megapixels) = asset.megapixels().filter(|mp| *mp >= 0.1) {
            size.push(format!("{megapixels:.1} MP"));
        }
        size.push(util::bytes(asset.size));
        size.push(asset.format.label().to_owned());
        rows.push((tr("Size"), size.join(" · ")));
        if let Some(duration) = asset.duration_ms {
            rows.push((tr("Length"), util::duration(duration)));
        }
        if let Some(camera) = asset.camera_name() {
            rows.push((tr("Camera"), camera));
        }
        if let Some(lens) = &asset.lens {
            rows.push((tr("Lens"), lens.clone()));
        }
        let mut exposure = Vec::new();
        if let Some(aperture) = asset.aperture {
            exposure.push(format!("ƒ/{}", trim_float(aperture)));
        }
        if let Some(seconds) = asset.exposure_seconds {
            exposure.push(if seconds > 0.0 && seconds < 1.0 {
                format!("1/{} s", (1.0 / seconds).round())
            } else {
                format!("{} s", trim_float(seconds))
            });
        }
        if let Some(iso) = asset.iso {
            exposure.push(format!("ISO {iso}"));
        }
        if let Some(focal) = asset.focal_length_mm {
            match asset.focal_length_35mm {
                Some(equivalent) if (equivalent - focal).abs() > 0.5 => {
                    exposure.push(format!(
                        "{} mm ({} mm eq.)",
                        trim_float(focal),
                        trim_float(equivalent)
                    ));
                }
                _ => exposure.push(format!("{} mm", trim_float(focal))),
            }
        }
        if !exposure.is_empty() {
            rows.push((tr("Exposure"), exposure.join(" · ")));
        }
        for (title, value) in rows {
            let row = adw::ActionRow::builder()
                .title(title)
                .subtitle(glib::markup_escape_text(&value))
                .subtitle_selectable(true)
                .css_classes(["property"])
                .build();
            self.details.add(&row);
            self.detail_rows.borrow_mut().push(row.upcast());
        }

        // Place.
        let has_place = asset.place.is_some() || asset.location.is_some();
        self.place.set_visible(has_place);
        if let Some(place) = &asset.place {
            let mut name = place.city.clone();
            if let Some(region) = &place.region {
                if !name.is_empty() && region != &name {
                    name.push_str(", ");
                    name.push_str(region);
                }
            }
            if !name.is_empty() {
                name.push_str(", ");
            }
            name.push_str(&place.country);
            self.place_row
                .set_subtitle(&glib::markup_escape_text(&name));
        } else {
            self.place_row.set_subtitle("");
        }
        if let Some(point) = asset.location {
            self.place_row
                .set_title(&format!("{:.5}, {:.5}", point.latitude, point.longitude));
            self.show_map(point.latitude, point.longitude);
        } else {
            self.place_row.set_title(&tr("Place"));
        }

        // Tags.
        self.tags_box.remove_all();
        for tag in &detail.tags {
            let chip = gtk::Box::builder()
                .spacing(4)
                .css_classes(["tag-chip"])
                .build();
            let name = gtk::Button::builder()
                .label(&tag.name)
                .css_classes(["flat", "small"])
                .tooltip_text(trf("Show Items Tagged “{}”", &[&tag.name]))
                .build();
            let remove = gtk::Button::builder()
                .icon_name("window-close-symbolic")
                .css_classes(["flat", "circular", "small"])
                .tooltip_text(tr("Remove Tag"))
                .build();
            chip.append(&name);
            chip.append(&remove);
            let ctx = Rc::clone(&self.ctx);
            let query = format!("tag:\"{}\"", tag.name);
            name.connect_clicked(move |_| {
                ctx.nav.pop();
                ctx.go(Destination::Search(query.clone()));
            });
            let ctx = Rc::clone(&self.ctx);
            let (asset_id, tag_id) = (asset.id, tag.id);
            remove.connect_clicked(move |_| {
                let ctx = Rc::clone(&ctx);
                util::spawn(async move {
                    if let Err(error) = ctx
                        .call(move |library| library.remove_tag(&[asset_id], tag_id))
                        .await
                    {
                        ctx.error(&tr("Could not remove the tag"), &error);
                    }
                });
            });
            self.tags_box.append(&chip);
        }

        // Albums.
        for row in self.album_rows.borrow_mut().drain(..) {
            self.albums.remove(&row);
        }
        self.albums.set_visible(!detail.albums.is_empty());
        for album in &detail.albums {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&album.name))
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("view-grid-symbolic"));
            self.albums.add(&row);
            self.album_rows.borrow_mut().push(row.upcast());
        }

        // Files.
        for row in self.file_rows.borrow_mut().drain(..) {
            self.files.remove(&row);
        }
        for file in &detail.files {
            let (kind, icon) = match file.source_kind {
                SourceKind::Managed => (tr("In the library"), "mn-library-symbolic"),
                SourceKind::Linked => (trf("In “{}”", &[&file.source_name]), "folder-symbolic"),
            };
            let status = match file.file.status {
                FileStatus::Verified => file
                    .file
                    .checked_ms
                    .map(|ms| trf("Verified {}", &[&util::ago(ms)]))
                    .unwrap_or_else(|| tr("Verified")),
                FileStatus::Unverified => tr("Not checked yet"),
                FileStatus::Missing => tr("Missing — reconnect the disk or folder"),
                FileStatus::Changed => tr("Changed on disk — see Review"),
            };
            let path = file.path().display().to_string();
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&kind))
                .subtitle(glib::markup_escape_text(&format!("{path}\n{status}")))
                .subtitle_selectable(true)
                .build();
            row.add_prefix(&gtk::Image::from_icon_name(icon));
            if matches!(file.file.status, FileStatus::Missing | FileStatus::Changed) {
                let warning = gtk::Image::from_icon_name("dialog-warning-symbolic");
                warning.add_css_class("warning");
                row.add_suffix(&warning);
            }
            self.files.add(&row);
            self.file_rows.borrow_mut().push(row.upcast());
        }
    }

    fn show_map(&self, latitude: f64, longitude: f64) {
        if !self.ctx.config.borrow().online_maps {
            return;
        }
        if self.map.borrow().is_none() {
            let map = shumate::SimpleMap::new();
            let registry = shumate::MapSourceRegistry::with_defaults();
            if let Some(source) = registry.by_id(shumate::MAP_SOURCE_OSM_MAPNIK) {
                map.set_map_source(Some(&source));
            }
            map.set_height_request(180);
            map.set_show_zoom_buttons(false);
            map.add_css_class("card");
            map.set_margin_top(6);
            let Some(viewport) = map.viewport() else {
                return;
            };
            let layer = shumate::MarkerLayer::new(&viewport);
            let marker = shumate::Marker::new();
            marker.set_child(Some(&gtk::Image::from_icon_name("mark-location-symbolic")));
            layer.add_marker(&marker);
            map.add_overlay_layer(&layer);
            self.place.add(&map);
            *self.marker.borrow_mut() = Some(marker);
            *self.map.borrow_mut() = Some(map);
        }
        if let (Some(map), Some(marker)) =
            (self.map.borrow().as_ref(), self.marker.borrow().as_ref())
        {
            marker.set_location(latitude, longitude);
            if let Some(inner) = map.map() {
                inner.center_on(latitude, longitude);
                if let Some(viewport) = inner.viewport() {
                    viewport.set_zoom_level(12.0);
                }
            }
        }
    }
}

/// `2.8` instead of `2.799999952316284`, `50` instead of `50.0`.
fn trim_float(value: f32) -> String {
    let text = format!("{value:.1}");
    text.strip_suffix(".0").map_or(text.clone(), str::to_owned)
}
