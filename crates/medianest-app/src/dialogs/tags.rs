//! Tagging: an input with suggestions, and a dialog for many items.

use std::rc::Rc;

use adw::prelude::*;
use medianest_service::core::AssetId;

use crate::context::Ctx;
use crate::i18n::{tr, trf, trn};
use crate::util::{self, Debounce};

/// A text field that suggests existing tags while typing.
pub struct TagInput {
    root: gtk::Box,
    entry: gtk::Entry,
    suggestions: gtk::FlowBox,
    debounce: Debounce,
}

impl TagInput {
    pub fn new(ctx: &Rc<Ctx>, on_add: impl Fn(String) + 'static) -> Rc<Self> {
        let entry = gtk::Entry::builder()
            .placeholder_text(tr("Add a tag…"))
            .secondary_icon_name("list-add-symbolic")
            .secondary_icon_tooltip_text(tr("Add Tag"))
            .build();
        let suggestions = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .column_spacing(6)
            .row_spacing(6)
            .max_children_per_line(6)
            .visible(false)
            .build();
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        root.append(&entry);
        root.append(&suggestions);
        let this = Rc::new(Self {
            root,
            entry,
            suggestions,
            debounce: Debounce::new(120),
        });
        let on_add: Rc<dyn Fn(String)> = Rc::new(on_add);

        let add = {
            let on_add = Rc::clone(&on_add);
            let weak = Rc::downgrade(&this);
            move || {
                let Some(this) = weak.upgrade() else { return };
                let name = this.entry.text().trim().to_owned();
                if name.is_empty() {
                    return;
                }
                this.entry.set_text("");
                on_add(name);
            }
        };
        let activate = add.clone();
        this.entry.connect_activate(move |_| activate());
        this.entry.connect_icon_release(move |_, _| add());

        let weak = Rc::downgrade(&this);
        let ctx = Rc::clone(ctx);
        this.entry.connect_changed(move |entry| {
            let Some(this) = weak.upgrade() else { return };
            let prefix = entry.text().trim().to_owned();
            if prefix.is_empty() {
                this.suggestions.remove_all();
                this.suggestions.set_visible(false);
                return;
            }
            let ctx = Rc::clone(&ctx);
            let weak = Rc::downgrade(&this);
            let on_add = Rc::clone(&on_add);
            this.debounce.call(move || {
                util::spawn(async move {
                    let query = prefix.clone();
                    let found = ctx
                        .call(move |library| library.read(|db| db.tag_suggestions(&query, 6)))
                        .await
                        .unwrap_or_default();
                    let Some(this) = weak.upgrade() else { return };
                    if this.entry.text().trim() != prefix {
                        return;
                    }
                    this.suggestions.remove_all();
                    let found: Vec<_> = found
                        .into_iter()
                        .filter(|facet| !facet.value.eq_ignore_ascii_case(&prefix))
                        .collect();
                    this.suggestions.set_visible(!found.is_empty());
                    for facet in found {
                        let chip = gtk::Button::builder()
                            .label(&facet.value)
                            .css_classes(["pill", "small", "tag-chip"])
                            .build();
                        let on_add = Rc::clone(&on_add);
                        let entry = this.entry.clone();
                        chip.connect_clicked(move |_| {
                            entry.set_text("");
                            on_add(facet.value.clone());
                        });
                        this.suggestions.append(&chip);
                    }
                });
            });
        });
        this
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    pub fn grab_focus(&self) {
        self.entry.grab_focus();
    }
}

/// Edits the tags of several items at once.
struct TagEditor {
    ctx: Rc<Ctx>,
    ids: Vec<AssetId>,
    chips: gtk::FlowBox,
    empty: gtk::Label,
}

impl TagEditor {
    fn refresh(self: &Rc<Self>) {
        let this = Rc::clone(self);
        util::spawn(async move {
            let query = this.ids.clone();
            let tags = this
                .ctx
                .call(move |library| library.read(|db| db.tags_in(&query)))
                .await
                .unwrap_or_default();
            this.chips.remove_all();
            this.empty.set_visible(tags.is_empty());
            let total = this.ids.len() as u64;
            for tag in tags {
                let label = if tag.count < total {
                    trf(
                        "{} ({} of {})",
                        &[&tag.name, &tag.count.to_string(), &total.to_string()],
                    )
                } else {
                    tag.name.clone()
                };
                let chip = gtk::Box::builder()
                    .spacing(2)
                    .css_classes(["tag-chip"])
                    .build();
                chip.append(&gtk::Label::builder().label(label).margin_start(10).build());
                let remove = gtk::Button::builder()
                    .icon_name("window-close-symbolic")
                    .tooltip_text(tr("Remove Tag"))
                    .css_classes(["flat", "circular", "small"])
                    .build();
                chip.append(&remove);
                let editor = Rc::clone(&this);
                remove.connect_clicked(move |_| {
                    let editor = Rc::clone(&editor);
                    let tag = tag.id;
                    util::spawn(async move {
                        let ids = editor.ids.clone();
                        if let Err(error) = editor
                            .ctx
                            .call(move |library| library.remove_tag(&ids, tag))
                            .await
                        {
                            editor.ctx.error(&tr("Could not remove the tag"), &error);
                        }
                        editor.refresh();
                    });
                });
                this.chips.append(&chip);
            }
        });
    }

    fn add(self: &Rc<Self>, name: String) {
        let editor = Rc::clone(self);
        util::spawn(async move {
            let ids = editor.ids.clone();
            if let Err(error) = editor
                .ctx
                .call(move |library| library.add_tag(&ids, &name))
                .await
            {
                editor.ctx.error(&tr("Could not add the tag"), &error);
            }
            editor.refresh();
        });
    }
}

pub fn show(ctx: &Rc<Ctx>, ids: Vec<AssetId>) {
    let count = ids.len() as u64;
    let editor = Rc::new(TagEditor {
        ctx: Rc::clone(ctx),
        ids,
        chips: gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .column_spacing(6)
            .row_spacing(6)
            .max_children_per_line(6)
            .build(),
        empty: gtk::Label::builder()
            .label(tr("No tags yet"))
            .css_classes(["dim-label"])
            .xalign(0.0)
            .build(),
    });
    let weak = Rc::downgrade(&editor);
    let input = TagInput::new(ctx, move |name| {
        if let Some(editor) = weak.upgrade() {
            editor.add(name);
        }
    });

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(18)
        .margin_end(18)
        .build();
    content.append(&editor.empty);
    content.append(&editor.chips);
    content.append(input.widget());
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&content));
    let dialog = adw::Dialog::builder()
        .title(trn("Tags of {} Item", "Tags of {} Items", count))
        .content_width(420)
        .child(&toolbar)
        .build();
    // The editor and input live as long as the dialog.
    let keep = (Rc::clone(&editor), Rc::clone(&input));
    dialog.connect_closed(move |_| {
        let _ = &keep;
    });
    editor.refresh();
    dialog.present(Some(&ctx.window));
    input.grab_focus();
}
