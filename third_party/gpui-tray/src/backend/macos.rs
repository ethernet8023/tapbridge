use std::cell::{Cell, RefCell};

use crate::menu::Shortcut;
use image::ImageEncoder as _;
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained,
};
use objc2_app_kit::{
    NSEventModifierFlags, NSControlStateValueOff, NSControlStateValueOn, NSImage, NSMenu, NSMenuItem, NSStatusBar,
    NSStatusBarButton, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSData, NSObject, NSObjectProtocol, NSSize, NSString};

use crate::{
    Error, Icon, Result,
    backend::BackendEvent,
    menu::{MenuItemId, NativeMenuItem},
    tray::TraySnapshot,
};

struct TargetIvars {
    events: async_channel::Sender<BackendEvent>,
    mappings: RefCell<Vec<(isize, u64, MenuItemId)>>,
    next_tag: Cell<isize>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. This object is main
    // thread-only and all of its ivars are initialized before `init` returns.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = TargetIvars]
    struct TrayTarget;

    // SAFETY: NSObjectProtocol has no additional invariants.
    unsafe impl NSObjectProtocol for TrayTarget {}

    impl TrayTarget {
        #[unsafe(method(trayActivated:))]
        fn tray_activated(&self, _sender: &NSStatusBarButton) {
            let _ = self.ivars().events.try_send(BackendEvent::Activated { token: None });
        }

        #[unsafe(method(trayMenuItemInvoked:))]
        fn tray_menu_item_invoked(&self, sender: &NSMenuItem) {
            let tag = sender.tag();
            if let Some((_, generation, id)) = self
                .ivars()
                .mappings
                .borrow()
                .iter()
                .find(|(candidate, _, _)| *candidate == tag)
            {
                let _ = self
                    .ivars()
                    .events
                    .try_send(BackendEvent::MenuItemClicked {
                        generation: *generation,
                        id: *id,
                    });
            }
        }
    }
);

impl TrayTarget {
    fn new(mtm: MainThreadMarker, events: async_channel::Sender<BackendEvent>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(TargetIvars {
            events,
            mappings: RefCell::new(Vec::new()),
            next_tag: Cell::new(1),
        });
        // SAFETY: NSObject's `init` has its declared signature.
        unsafe { msg_send![super(this), init] }
    }

    fn allocate_tag(&self, generation: u64, id: MenuItemId) -> isize {
        let tag = self.ivars().next_tag.get();
        self.ivars().next_tag.set(tag.saturating_add(1).max(1));
        self.ivars()
            .mappings
            .borrow_mut()
            .push((tag, generation, id));
        tag
    }

    fn reset_mappings(&self) {
        self.ivars().mappings.borrow_mut().clear();
    }
}

pub(crate) struct MacTray {
    mtm: MainThreadMarker,
    status_bar: Retained<NSStatusBar>,
    status_item: Retained<NSStatusItem>,
    target: Retained<TrayTarget>,
    menu: Option<Retained<NSMenu>>,
    image: Option<Retained<NSImage>>,
    closed: bool,
}

impl MacTray {
    pub(crate) fn new(
        snapshot: &TraySnapshot,
        events: async_channel::Sender<BackendEvent>,
    ) -> Result<Self> {
        let mtm = MainThreadMarker::new().ok_or(Error::WrongThread)?;
        let status_bar = NSStatusBar::systemStatusBar();
        let status_item = status_bar.statusItemWithLength(NSVariableStatusItemLength);
        let target = TrayTarget::new(mtm, events);
        let mut tray = Self {
            mtm,
            status_bar,
            status_item,
            target,
            menu: None,
            image: None,
            closed: false,
        };
        tray.apply_snapshot(snapshot)?;
        Ok(tray)
    }

    pub(crate) fn apply(&mut self, old: &TraySnapshot, new: &TraySnapshot) -> Result<()> {
        if self.closed {
            return Err(Error::Closed);
        }
        if old.icon != new.icon {
            self.set_icon(new.icon.as_ref())?;
        }
        if old.title != new.title || old.tooltip != new.tooltip {
            self.set_button_text(new);
        }
        if old.visible != new.visible {
            self.status_item.setVisible(new.visible);
        }
        if old.activatable != new.activatable {
            self.set_activation(new.activatable);
        }
        if old.menu != new.menu {
            self.set_menu(new)?;
        }
        Ok(())
    }

    pub(crate) fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.status_item.setMenu(None);
        self.set_activation(false);
        self.status_bar.removeStatusItem(&self.status_item);
        self.target.reset_mappings();
        self.menu = None;
        self.image = None;
        self.closed = true;
        Ok(())
    }

    fn apply_snapshot(&mut self, snapshot: &TraySnapshot) -> Result<()> {
        self.set_icon(snapshot.icon.as_ref())?;
        self.set_button_text(snapshot);
        self.set_activation(snapshot.activatable);
        self.status_item.setVisible(snapshot.visible);
        self.set_menu(snapshot)
    }

    fn set_activation(&self, activatable: bool) {
        let Some(button) = self.status_item.button(self.mtm) else {
            return;
        };
        // SAFETY: `TrayTarget` implements the selected action method and is
        // retained by `MacTray` for at least as long as the status button.
        unsafe {
            if activatable {
                button.setTarget(Some(self.target.as_ref()));
                button.setAction(Some(objc2::sel!(trayActivated:)));
            } else {
                button.setAction(None);
                button.setTarget(None);
            }
        }
    }

    fn set_button_text(&self, snapshot: &TraySnapshot) {
        if let Some(button) = self.status_item.button(self.mtm) {
            button.setTitle(&NSString::from_str(
                snapshot.title.as_deref().unwrap_or_default(),
            ));
            let tooltip = snapshot.tooltip.as_deref().map(NSString::from_str);
            button.setToolTip(tooltip.as_deref());
        }
    }

    fn set_icon(&mut self, icon: Option<&Icon>) -> Result<()> {
        self.image = icon.map(native_image).transpose()?;
        if let Some(button) = self.status_item.button(self.mtm) {
            button.setImage(self.image.as_deref());
        }
        Ok(())
    }

    fn set_menu(&mut self, tray: &TraySnapshot) -> Result<()> {
        self.target.reset_mappings();
        self.menu = tray
            .menu
            .as_ref()
            .map(|menu| {
                build_menu(
                    "gpui-tray",
                    &menu.items,
                    &tray.item_images,
                    menu.generation,
                    &self.target,
                    self.mtm,
                )
            })
            .transpose()?;
        self.status_item.setMenu(self.menu.as_deref());
        Ok(())
    }
}

impl Drop for MacTray {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn native_image(icon: &Icon) -> Result<Retained<NSImage>> {
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            icon.rgba(),
            icon.width(),
            icon.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(Error::native)?;
    // SAFETY: NSData copies `png`, which remains valid for the duration of the call.
    let data = unsafe { NSData::dataWithBytes_length(png.as_ptr().cast(), png.len()) };
    let image = NSImage::initWithData(NSImage::alloc(), &data)
        .ok_or_else(|| Error::native_message("AppKit rejected tray icon data"))?;
    if icon.is_template() {
        // Local patch (see `Icon::menu_bar_template`): the system recolours a
        // template for the menu bar, and a PNG's pixel size is its point size
        // unless set, which would be twice the menu-bar height at @2x.
        image.setTemplate(true);
        let scale = MENU_BAR_ICON_HEIGHT / f64::from(icon.height().max(1));
        image.setSize(NSSize::new(f64::from(icon.width()) * scale, MENU_BAR_ICON_HEIGHT));
    }
    Ok(image)
}

/// Shows `shortcut` as the item's key equivalent. Only a single character is a
/// key equivalent (Return, F-keys and friends would need their AppKit codes).
/// Local patch: not in upstream gpui-tray 0.1.4.
fn set_key_equivalent(item: &NSMenuItem, shortcut: &Shortcut) {
    let mut chars = shortcut.key.chars();
    let (Some(key), None) = (chars.next(), chars.next()) else {
        return;
    };
    let mut flags = NSEventModifierFlags::empty();
    if shortcut.command {
        flags |= NSEventModifierFlags::Command;
    }
    if shortcut.control {
        flags |= NSEventModifierFlags::Control;
    }
    if shortcut.option {
        flags |= NSEventModifierFlags::Option;
    }
    if shortcut.shift {
        flags |= NSEventModifierFlags::Shift;
    }
    item.setKeyEquivalent(&NSString::from_str(&key.to_lowercase().to_string()));
    item.setKeyEquivalentModifierMask(flags);
}

/// An SVG as a template image about the size of an SF Symbol at the menu font
/// size (these exports are tight to the glyph), keeping its aspect ratio.
/// NSImage reads SVG itself (macOS 13+). Local patch.
fn svg_menu_image(svg: &[u8]) -> Option<Retained<NSImage>> {
    // SAFETY: NSData copies `svg`, which remains valid for the duration of the call.
    let data = unsafe { NSData::dataWithBytes_length(svg.as_ptr().cast(), svg.len()) };
    let image = NSImage::initWithData(NSImage::alloc(), &data)?;
    let natural = image.size();
    let scale = 13.0 / natural.width.max(natural.height).max(1.0);
    image.setSize(NSSize::new(natural.width * scale, natural.height * scale));
    image.setTemplate(true);
    Some(image)
}

/// Menu-bar icon height in points (what tray-icon used).
const MENU_BAR_ICON_HEIGHT: f64 = 22.0;

fn build_menu(
    title: &str,
    items: &[NativeMenuItem],
    images: &[(String, std::sync::Arc<[u8]>)],
    generation: u64,
    target: &TrayTarget,
    mtm: MainThreadMarker,
) -> Result<Retained<NSMenu>> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
    // Without this AppKit re-enables every item whose target answers its
    // selector, so a disabled status line would be clickable. Local patch.
    menu.setAutoenablesItems(false);
    for item in items {
        match item {
            NativeMenuItem::Separator => menu.addItem(&NSMenuItem::separatorItem(mtm)),
            NativeMenuItem::Item {
                id,
                label,
                checked,
                enabled,
                shortcut,
            } => {
                // SAFETY: The selector is implemented by TrayTarget and target is retained by MacTray.
                let item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(mtm),
                        &NSString::from_str(label),
                        Some(objc2::sel!(trayMenuItemInvoked:)),
                        &NSString::new(),
                    )
                };
                item.setTag(target.allocate_tag(generation, *id));
                item.setEnabled(*enabled);
                if let Some(shortcut) = shortcut {
                    set_key_equivalent(&item, shortcut);
                }
                if let Some((_, svg)) = images.iter().find(|(name, _)| name == label) {
                    if let Some(image) = svg_menu_image(svg) {
                        item.setImage(Some(&image));
                    }
                }
                item.setState(if *checked {
                    NSControlStateValueOn
                } else {
                    NSControlStateValueOff
                });
                // SAFETY: `target` outlives every menu built here.
                unsafe { item.setTarget(Some(target)) };
                menu.addItem(&item);
            }
            NativeMenuItem::Submenu {
                label,
                enabled,
                items,
            } => {
                // SAFETY: A submenu container has no action selector.
                let item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(mtm),
                        &NSString::from_str(label),
                        None,
                        &NSString::new(),
                    )
                };
                item.setEnabled(*enabled);
                let submenu = build_menu(label, items, images, generation, target, mtm)?;
                item.setSubmenu(Some(&submenu));
                menu.addItem(&item);
            }
        }
    }
    Ok(menu)
}
