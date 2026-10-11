//! Places: a map of where photos were taken, and a list of countries and
//! cities. Place names come from an offline database; map images are
//! downloaded only when online maps are allowed.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::f64::consts::PI;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use medianest_service::Event;
use medianest_service::catalog::{Facet, MapPoint, View};
use medianest_service::core::query::Query;
use medianest_service::core::{AssetId, AssetUuid};
use shumate::prelude::*;

use super::Section;
use super::timeline::{TimelineKind, TimelineOptions, TimelinePage};
use crate::context::Ctx;
use crate::i18n::tr;
use crate::thumb::Thumb;
use crate::util::{self, Debounce};

/// Points closer than this on screen join one marker.
const CLUSTER_PIXELS: f64 = 72.0;
const MARKER_SIZE: i32 = 56;

struct Cluster {
    latitude: f64,
    longitude: f64,
    count: u64,
    cover: AssetId,
}

pub struct PlacesPage {
    this: std::rc::Weak<Self>,
    ctx: Rc<Ctx>,
    nav: adw::NavigationView,
    map: Option<shumate::Map>,
    layer: Option<shumate::MarkerLayer>,
    countries: gtk::ListBox,
    cities: gtk::ListBox,
    stack: gtk::Stack,
    points: RefCell<Vec<MapPoint>>,
    uuids: RefCell<HashMap<AssetId, AssetUuid>>,
    markers: RefCell<Vec<(shumate::Marker, Thumb, AssetId)>>,
    zoom: Cell<i32>,
    stale: Cell<bool>,
    debounce: Debounce,
}

impl PlacesPage {
    pub fn new(ctx: Rc<Ctx>, toggle: gtk::ToggleButton) -> Rc<Self> {
        let online = ctx.config.borrow().online_maps;
        let header = adw::HeaderBar::new();
        header.pack_start(&toggle);

        let countries = gtk::ListBox::builder()
            .css_classes(["boxed-list"])
            .selection_mode(gtk::SelectionMode::None)
            .build();
        let cities = gtk::ListBox::builder()
            .css_classes(["boxed-list"])
            .selection_mode(gtk::SelectionMode::None)
            .build();
        let list_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(12)
            .margin_end(12)
            .build();
        list_content.append(
            &gtk::Label::builder()
                .label(tr("Countries"))
                .xalign(0.0)
                .css_classes(["heading"])
                .build(),
        );
        list_content.append(&countries);
        list_content.append(
            &gtk::Label::builder()
                .label(tr("Cities"))
                .xalign(0.0)
                .margin_top(12)
                .css_classes(["heading"])
                .build(),
        );
        list_content.append(&cities);
        let list_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&list_content)
            .build();

        let (map, layer, content): (
            Option<shumate::Map>,
            Option<shumate::MarkerLayer>,
            gtk::Widget,
        ) = if online {
            let simple = shumate::SimpleMap::new();
            let registry = shumate::MapSourceRegistry::with_defaults();
            if let Some(source) = registry.by_id(shumate::MAP_SOURCE_OSM_MAPNIK) {
                simple.set_map_source(Some(&source));
            }
            simple.set_show_zoom_buttons(true);
            simple.set_hexpand(true);
            simple.set_vexpand(true);
            let map = simple.map();
            let layer = simple.viewport().map(|viewport| {
                let layer = shumate::MarkerLayer::new(&viewport);
                simple.add_overlay_layer(&layer);
                layer
            });
            if let Some(viewport) = simple.viewport() {
                viewport.set_zoom_level(2.0);
            }
            (map, layer, simple.upcast())
        } else {
            let status = adw::StatusPage::builder()
                .icon_name("mn-map-symbolic")
                .title(tr("Maps Are Off"))
                .description(tr("Turn on online maps in Preferences to see photos on a map. The list of places works offline."))
                .build();
            (None, None, status.upcast())
        };
        let split = adw::OverlaySplitView::builder()
            .content(&content)
            .sidebar(&list_scroller)
            .sidebar_position(gtk::PackType::End)
            .min_sidebar_width(260.0)
            .max_sidebar_width(340.0)
            .build();
        let list_toggle = gtk::ToggleButton::builder()
            .icon_name("view-list-symbolic")
            .tooltip_text(tr("Show List"))
            .build();
        split
            .bind_property("show-sidebar", &list_toggle, "active")
            .bidirectional()
            .sync_create()
            .build();
        header.pack_end(&list_toggle);

        let empty = adw::StatusPage::builder()
            .icon_name("mn-map-symbolic")
            .title(tr("No Places Yet"))
            .description(tr(
                "Photos with a location appear here. Most phones record where a photo was taken.",
            ))
            .build();
        let stack = gtk::Stack::new();
        stack.add_named(&split, Some("places"));
        stack.add_named(&empty, Some("empty"));

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));
        let nav = adw::NavigationView::new();
        nav.add(
            &adw::NavigationPage::builder()
                .title(tr("Places"))
                .tag("places")
                .child(&toolbar)
                .build(),
        );

        let this = Rc::new_cyclic(|weak| Self {
            this: weak.clone(),
            ctx,
            nav,
            map,
            layer,
            countries,
            cities,
            stack,
            points: RefCell::default(),
            uuids: RefCell::default(),
            markers: RefCell::default(),
            zoom: Cell::new(-1),
            stale: Cell::new(true),
            debounce: Debounce::new(400),
        });
        if let Some(viewport) = this.map.as_ref().and_then(shumate::Map::viewport) {
            let weak = this.this.clone();
            viewport.connect_zoom_level_notify(move |viewport| {
                if let Some(this) = weak.upgrade() {
                    #[allow(clippy::cast_possible_truncation)]
                    let zoom = viewport.zoom_level().floor() as i32;
                    if zoom != this.zoom.get() {
                        this.zoom.set(zoom);
                        this.cluster();
                    }
                }
            });
        }
        let weak = this.this.clone();
        this.ctx.loader.connect_loaded(move || {
            if let Some(this) = weak.upgrade() {
                this.refresh_covers();
            }
        });
        this
    }

    fn reload(&self) {
        self.stale.set(false);
        let ctx = Rc::clone(&self.ctx);
        let weak = self.this.clone();
        util::spawn(async move {
            let data = ctx
                .call(|library| {
                    library.read(|db| {
                        let view = View::library();
                        let points = db.map_points(&view)?;
                        let uuids: HashMap<AssetId, AssetUuid> = db
                            .timeline(&view)?
                            .into_iter()
                            .map(|item| (item.id, item.uuid))
                            .collect();
                        Ok((points, uuids, db.countries()?, db.places("", 200)?))
                    })
                })
                .await;
            let Some(this) = weak.upgrade() else { return };
            match data {
                Ok((points, uuids, countries, cities)) => {
                    let empty = points.is_empty() && countries.is_empty();
                    this.stack
                        .set_visible_child_name(if empty { "empty" } else { "places" });
                    *this.points.borrow_mut() = points;
                    *this.uuids.borrow_mut() = uuids;
                    this.fill_list(&this.countries, countries, true);
                    this.fill_list(&this.cities, cities, false);
                    this.cluster();
                }
                Err(error) => this.ctx.error(&tr("Could not load places"), &error),
            }
        });
    }

    fn fill_list(&self, list: &gtk::ListBox, facets: Vec<Facet>, countries: bool) {
        while let Some(row) = list.first_child() {
            list.remove(&row);
        }
        for facet in facets {
            let title = if facet.value.is_empty() {
                tr("Unknown")
            } else {
                facet.value.clone()
            };
            let subtitle = if countries {
                util::items(facet.count)
            } else {
                format!(
                    "{} · {}",
                    facet.detail.clone().unwrap_or_default(),
                    util::items(facet.count)
                )
            };
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&title))
                .subtitle(glib::markup_escape_text(&subtitle))
                .activatable(true)
                .build();
            if countries {
                if let Some(code) = facet.detail.as_deref() {
                    row.add_prefix(&gtk::Label::new(Some(&flag(code))));
                }
            }
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let weak = self.this.clone();
            row.connect_activated(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.open_place(&title);
                }
            });
            list.append(&row);
        }
    }

    fn open_place(&self, name: &str) {
        let query = format!("place:\"{}\"", name.replace('"', ""));
        let page = TimelinePage::new(
            Rc::clone(&self.ctx),
            TimelineOptions {
                title: name.to_owned(),
                view: View::library().with_query(Query::parse(&query)),
                kind: TimelineKind::Filtered,
                searchable: false,
                empty_icon: "mn-map-symbolic",
                empty_title: tr("No Photos Here"),
                empty_description: String::new(),
                sidebar_toggle: None,
            },
        );
        self.nav.push(&page.navigation_page());
    }

    /// Groups points that would overlap at the current zoom level.
    fn cluster(&self) {
        let (Some(map), Some(layer)) = (&self.map, &self.layer) else {
            return;
        };
        let zoom = map.viewport().map_or(2.0, |viewport| viewport.zoom_level());
        let world = 256.0 * 2f64.powf(zoom.floor());
        let mut cells: HashMap<(i64, i64), (f64, f64, u64, AssetId)> = HashMap::new();
        for point in self.points.borrow().iter() {
            let (x, y) = project(point.latitude, point.longitude, world);
            #[allow(clippy::cast_possible_truncation)]
            let key = (
                (x / CLUSTER_PIXELS).floor() as i64,
                (y / CLUSTER_PIXELS).floor() as i64,
            );
            let entry = cells.entry(key).or_insert((0.0, 0.0, 0, point.id));
            entry.0 += point.latitude;
            entry.1 += point.longitude;
            entry.2 += 1;
        }
        let clusters: Vec<Cluster> = cells
            .into_values()
            .map(|(lat, lon, count, cover)| {
                #[allow(clippy::cast_precision_loss)]
                let n = count as f64;
                Cluster {
                    latitude: lat / n,
                    longitude: lon / n,
                    count,
                    cover,
                }
            })
            .collect();
        layer.remove_all();
        self.markers.borrow_mut().clear();
        let mut wanted = Vec::new();
        for cluster in clusters {
            let (marker, picture) = self.marker(&cluster);
            layer.add_marker(&marker);
            if let Some(uuid) = self.uuids.borrow().get(&cluster.cover) {
                wanted.push((cluster.cover, *uuid));
            }
            self.markers
                .borrow_mut()
                .push((marker, picture, cluster.cover));
        }
        self.ctx.loader.want(&wanted);
        self.refresh_covers();
    }

    fn marker(&self, cluster: &Cluster) -> (shumate::Marker, Thumb) {
        let picture = Thumb::new(MARKER_SIZE, 10.0);
        picture.add_css_class("map-marker");
        let overlay = gtk::Overlay::builder().child(&picture).build();
        if cluster.count > 1 {
            let badge = gtk::Label::builder()
                .label(util::number(cluster.count))
                .halign(gtk::Align::End)
                .valign(gtk::Align::Start)
                .css_classes(["map-count", "numeric"])
                .build();
            overlay.add_overlay(&badge);
        }
        let button = gtk::Button::builder()
            .child(&overlay)
            .css_classes(["flat", "map-marker-button"])
            .tooltip_text(util::items(cluster.count))
            .build();
        let marker = shumate::Marker::builder().child(&button).build();
        marker.set_location(cluster.latitude, cluster.longitude);
        let weak = self.this.clone();
        let (latitude, longitude, cover) = (cluster.latitude, cluster.longitude, cluster.cover);
        button.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.marker_clicked(latitude, longitude, cover);
            }
        });
        (marker, picture)
    }

    fn marker_clicked(&self, latitude: f64, longitude: f64, cover: AssetId) {
        let Some(map) = &self.map else { return };
        let zoom = map.viewport().map_or(0.0, |viewport| viewport.zoom_level());
        if zoom < 13.0 {
            map.go_to_full_with_duration(latitude, longitude, (zoom + 3.0).min(16.0), 400);
            return;
        }
        let ctx = Rc::clone(&self.ctx);
        let weak = self.this.clone();
        util::spawn(async move {
            let place = ctx
                .call(move |library| Ok(library.read(|db| db.asset(cover))?.place))
                .await;
            if let (Some(this), Ok(Some(place))) = (weak.upgrade(), place) {
                let name = if place.city.is_empty() {
                    place.country
                } else {
                    place.city
                };
                this.open_place(&name);
            }
        });
    }

    fn refresh_covers(&self) {
        let uuids = self.uuids.borrow();
        for (_, picture, cover) in self.markers.borrow().iter() {
            if picture.has_texture() {
                continue;
            }
            if let Some(texture) = uuids.get(cover).and_then(|uuid| self.ctx.loader.get(*uuid)) {
                picture.set_texture(Some(&texture));
            }
        }
    }
}

/// Web Mercator pixel coordinates.
fn project(latitude: f64, longitude: f64, world: f64) -> (f64, f64) {
    let x = (longitude + 180.0) / 360.0 * world;
    let sin = (latitude.clamp(-85.0, 85.0) * PI / 180.0).sin();
    let y = (0.5 - ((1.0 + sin) / (1.0 - sin)).ln() / (4.0 * PI)) * world;
    (x, y)
}

/// A country code as a flag emoji.
fn flag(code: &str) -> String {
    code.to_ascii_uppercase()
        .chars()
        .filter(char::is_ascii_uppercase)
        .filter_map(|c| char::from_u32(0x1F1E6 + (c as u32 - 'A' as u32)))
        .collect()
}

impl Section for PlacesPage {
    fn widget(&self) -> gtk::Widget {
        self.nav.clone().upcast()
    }

    fn shown(&self) {
        if self.stale.get() {
            self.reload();
        }
    }

    fn event(&self, event: &Event, visible: bool) {
        if matches!(event, Event::ContentChanged) {
            self.stale.set(true);
            if visible {
                let weak = self.this.clone();
                self.debounce.call(move || {
                    if let Some(this) = weak.upgrade() {
                        this.reload();
                    }
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_and_projection() {
        assert_eq!(flag("fr"), "🇫🇷");
        let (x, y) = project(0.0, 0.0, 256.0);
        assert!((x - 128.0).abs() < 1e-9 && (y - 128.0).abs() < 1e-9);
    }
}
