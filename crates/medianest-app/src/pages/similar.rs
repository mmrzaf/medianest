//! Similar photos: bursts and near-duplicates, best first, with the
//! others suggested for the trash.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use adw::prelude::*;
use medianest_service::catalog::Asset;
use medianest_service::core::AssetId;

use crate::actions;
use crate::context::Ctx;
use crate::i18n::{tr, trn};
use crate::textures;
use crate::thumb::Thumb;
use crate::util;

const THUMB: i32 = 132;
/// Groups shown at once; the rest come after these are handled.
const PAGE: usize = 60;

pub struct SimilarPage {
    this: std::rc::Weak<Self>,
    ctx: Rc<Ctx>,
    root: adw::ToolbarView,
    list: gtk::Box,
    stack: gtk::Stack,
    status: adw::StatusPage,
    /// Assets chosen for the trash.
    chosen: RefCell<HashSet<AssetId>>,
    trash_all: gtk::Button,
}

impl SimilarPage {
    pub fn new(ctx: Rc<Ctx>) -> Rc<Self> {
        let trash_all = gtk::Button::builder()
            .label(tr("_Trash Selected"))
            .use_underline(true)
            .css_classes(["destructive-action"])
            .sensitive(false)
            .build();
        let header = adw::HeaderBar::new();
        header.pack_end(&trash_all);
        let list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        let clamp = adw::Clamp::builder().maximum_size(900).child(&list).build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&clamp)
            .build();
        let status = adw::StatusPage::builder()
            .paintable(&adw::SpinnerPaintable::new(None::<&gtk::Widget>))
            .title(tr("Comparing Photos…"))
            .build();
        let stack = gtk::Stack::new();
        stack.add_named(&status, Some("status"));
        stack.add_named(&scroller, Some("groups"));
        let root = adw::ToolbarView::new();
        root.add_top_bar(&header);
        root.set_content(Some(&stack));
        let this = Rc::new_cyclic(|weak| Self {
            this: weak.clone(),
            ctx,
            root,
            list,
            stack,
            status,
            chosen: RefCell::default(),
            trash_all,
        });
        let weak = this.this.clone();
        this.trash_all.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.trash_chosen();
            }
        });
        this.reload();
        this
    }

    pub fn navigation_page(self: &Rc<Self>) -> adw::NavigationPage {
        let page = adw::NavigationPage::builder()
            .title(tr("Similar Photos"))
            .child(&self.root)
            .build();
        let keep = Rc::clone(self);
        page.connect_hidden(move |_| {
            let _ = &keep;
        });
        page
    }

    fn reload(&self) {
        self.stack.set_visible_child_name("status");
        let ctx = Rc::clone(&self.ctx);
        let weak = self.this.clone();
        util::spawn(async move {
            let groups = ctx
                .call(|library| {
                    let groups = library.similar_groups()?;
                    let ids: Vec<AssetId> = groups
                        .iter()
                        .take(PAGE)
                        .flat_map(|g| g.assets.clone())
                        .collect();
                    let assets: HashMap<AssetId, Asset> = library
                        .read(|db| db.assets(&ids))?
                        .into_iter()
                        .map(|a| (a.id, a))
                        .collect();
                    let total = groups.len();
                    let groups: Vec<Vec<(Asset, Option<textures::Pixels>)>> = groups
                        .into_iter()
                        .take(PAGE)
                        .map(|group| {
                            group
                                .assets
                                .iter()
                                .filter_map(|id| assets.get(id).cloned())
                                .map(|asset| {
                                    let pixels =
                                        textures::decode_jpeg(&library.thumbnail_path(asset.uuid))
                                            .ok();
                                    (asset, pixels)
                                })
                                .collect()
                        })
                        .collect();
                    Ok((groups, total))
                })
                .await;
            let Some(this) = weak.upgrade() else { return };
            match groups {
                Ok((groups, total)) => this.show(groups, total),
                Err(error) => this.ctx.error(&tr("Could not compare photos"), &error),
            }
        });
    }

    fn show(&self, groups: Vec<Vec<(Asset, Option<textures::Pixels>)>>, total: usize) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.chosen.borrow_mut().clear();
        if groups.is_empty() {
            self.status.set_paintable(None::<&gtk::gdk::Paintable>);
            self.status.set_icon_name(Some("mn-check-badge-symbolic"));
            self.status.set_title(&tr("No Similar Photos"));
            self.status.set_description(Some(&tr(
                "Nothing in your library looks like a near-duplicate.",
            )));
            self.stack.set_visible_child_name("status");
            self.update_button();
            return;
        }
        let intro = gtk::Label::builder()
            .label(if total > PAGE {
                trn(
                    "{} group of similar photos. The best of each is kept; the others are selected for the trash. Showing the first 60.",
                    "{} groups of similar photos. The best of each is kept; the others are selected for the trash. Showing the first 60.",
                    total as u64,
                )
            } else {
                trn(
                    "{} group of similar photos. The best of each is kept; the others are selected for the trash.",
                    "{} groups of similar photos. The best of each is kept; the others are selected for the trash.",
                    total as u64,
                )
            })
            .wrap(true)
            .xalign(0.0)
            .css_classes(["dim-label"])
            .build();
        self.list.append(&intro);
        for group in groups {
            self.list.append(&self.group_widget(group));
        }
        self.stack.set_visible_child_name("groups");
        self.update_button();
    }

    fn group_widget(&self, group: Vec<(Asset, Option<textures::Pixels>)>) -> gtk::Widget {
        let flow = gtk::Box::builder().spacing(12).build();
        let ids: Vec<AssetId> = group.iter().map(|(asset, _)| asset.id).collect();
        for (index, (asset, pixels)) in group.into_iter().enumerate() {
            let picture = Thumb::new(THUMB, 10.0);
            picture.set_texture(pixels.map(textures::Pixels::texture).as_ref());
            picture.set_quarter_turns(u8::try_from((asset.rotation % 360) / 90).unwrap_or(0));
            let best = index == 0;
            let check = gtk::CheckButton::builder()
                .label(tr("Move to Trash"))
                .active(!best)
                .build();
            if !best {
                self.chosen.borrow_mut().insert(asset.id);
            }
            let mut details = Vec::new();
            if let Some((w, h)) = asset.display_size() {
                details.push(format!("{w}×{h}"));
            }
            details.push(util::bytes(asset.size));
            let detail = gtk::Label::builder()
                .label(details.join(" · "))
                .css_classes(["caption", "dim-label", "numeric"])
                .build();
            let column = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(6)
                .build();
            let frame = gtk::Overlay::builder().child(&picture).build();
            if best {
                let badge = gtk::Label::builder()
                    .label(tr("Best"))
                    .halign(gtk::Align::Start)
                    .valign(gtk::Align::Start)
                    .margin_start(6)
                    .margin_top(6)
                    .css_classes(["osd", "live-badge"])
                    .build();
                frame.add_overlay(&badge);
            }
            column.append(&frame);
            column.append(&detail);
            column.append(&check);
            let (weak, id) = (self.this.clone(), asset.id);
            check.connect_toggled(move |check| {
                if let Some(this) = weak.upgrade() {
                    if check.is_active() {
                        this.chosen.borrow_mut().insert(id);
                    } else {
                        this.chosen.borrow_mut().remove(&id);
                    }
                    this.update_button();
                }
            });
            flow.append(&column);
        }
        let scroller = gtk::ScrolledWindow::builder()
            .vscrollbar_policy(gtk::PolicyType::Never)
            .child(&flow)
            .build();
        let not_similar = gtk::Button::builder()
            .label(tr("_Not Similar"))
            .use_underline(true)
            .halign(gtk::Align::Start)
            .css_classes(["flat"])
            .build();
        let card = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .css_classes(["card"])
            .build();
        let inner = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        inner.append(&scroller);
        inner.append(&not_similar);
        card.append(&inner);
        let (weak, card_ref) = (self.this.clone(), card.clone());
        not_similar.connect_clicked(move |_| {
            let Some(this) = weak.upgrade() else { return };
            for id in &ids {
                this.chosen.borrow_mut().remove(id);
            }
            this.update_button();
            this.list.remove(&card_ref);
            let ctx = Rc::clone(&this.ctx);
            let ids = ids.clone();
            util::spawn(async move {
                let _ = ctx
                    .call(move |library| {
                        for (i, a) in ids.iter().enumerate() {
                            for b in &ids[i + 1..] {
                                library.dismiss_similar(*a, *b)?;
                            }
                        }
                        Ok(())
                    })
                    .await;
            });
        });
        card.upcast()
    }

    fn update_button(&self) {
        let count = self.chosen.borrow().len() as u64;
        self.trash_all.set_sensitive(count > 0);
        self.trash_all.set_label(&if count > 0 {
            trn("_Trash {} Photo", "_Trash {} Photos", count)
        } else {
            tr("_Trash Selected")
        });
    }

    fn trash_chosen(&self) {
        let ids: Vec<AssetId> = self.chosen.borrow().iter().copied().collect();
        if ids.is_empty() {
            return;
        }
        actions::trash(&self.ctx, ids);
        // Look again once the trash has been updated.
        let weak = self.this.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(800), move || {
            if let Some(this) = weak.upgrade() {
                this.reload();
            }
        });
    }
}
