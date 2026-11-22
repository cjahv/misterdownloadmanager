//! AppKit reads the current command tree between frames, when a menu opens.
//! Selections go back through the application's regular command dispatcher.
#![allow(unsafe_code)]

use crate::app::{App, Cmd};
use crate::chrome::MenuEntry;
use crate::{chrome, i18n};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSMenu, NSMenuDelegate, NSMenuItem};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSString};
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::mpsc::{self, Receiver, Sender};

pub struct State {
    app: Weak<RefCell<App>>,
    ctx: egui::Context,
    send: Sender<Cmd>,
    section: Option<usize>,
    system: bool,
    commands: RefCell<Vec<Cmd>>,
    previous: RefCell<Option<(&'static str, Vec<MenuEntry<Cmd>>)>>,
}

define_class!(
    // SAFETY: AppKit owns the main thread. NSMenu's delegate and NSMenuItem's target
    // are weak; their owner below keeps every handler alive for its menu's lifetime.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MDMMainMenuHandler"]
    #[ivars = State]
    struct Handler;

    unsafe impl NSObjectProtocol for Handler {}
    unsafe impl NSMenuDelegate for Handler {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            self.refresh(menu);
        }
    }
    impl Handler {
        #[unsafe(method(mdmChoose:))]
        fn choose(&self, sender: &NSMenuItem) {
            let state = self.ivars();
            if let Some(cmd) = state.commands.borrow().get(sender.tag() as usize) {
                let _ = state.send.send(*cmd);
                state.ctx.request_repaint();
            }
        }
    }
);

impl Handler {
    fn refresh(&self, menu: &NSMenu) {
        let state = self.ivars();
        let Some(app) = state.app.upgrade() else {
            return;
        };
        let (Some(section), Ok(app)) = (state.section, app.try_borrow()) else {
            return;
        };
        let entries = app.menu_entries(section);
        let lang = i18n::code();
        let Ok(mut previous) = state.previous.try_borrow_mut() else {
            return;
        };
        if previous
            .as_ref()
            .is_some_and(|(code, old)| *code == lang && old == &entries)
        {
            return;
        }
        menu.removeAllItems();
        state.commands.borrow_mut().clear();
        self.fill(menu, &entries);
        *previous = Some((lang, entries));
    }

    fn item(&self, text: &str, key: &str, cmd: Cmd) -> Retained<NSMenuItem> {
        // SAFETY: mdmChoose: takes the sending NSMenuItem and is implemented above.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                self.mtm().alloc(),
                &NSString::from_str(text),
                Some(sel!(mdmChoose:)),
                &NSString::from_str(key),
            )
        };
        let mut commands = self.ivars().commands.borrow_mut();
        item.setTag(commands.len() as isize);
        commands.push(cmd);
        unsafe { item.setTarget(Some(self)) };
        item
    }

    fn fill(&self, menu: &NSMenu, entries: &[MenuEntry<Cmd>]) {
        let mtm = self.mtm();
        menu.setAutoenablesItems(false);
        // Delay a separator so moving Settings and Quit to the app menu leaves no trailing rule.
        let mut divider = false;
        for entry in entries {
            let item = match entry {
                MenuEntry::Sep => {
                    divider = menu.numberOfItems() > 0;
                    continue;
                }
                MenuEntry::Item(_, Cmd::Exit | Cmd::Options, ..) if self.ivars().system => continue,
                MenuEntry::Item(label, cmd, enabled, checked)
                | MenuEntry::Radio(label, cmd, enabled, checked) => {
                    let text = chrome::mac_caption(i18n::tc("menu", label));
                    let item = self.item(&text, "", *cmd);
                    item.setEnabled(*enabled);
                    item.setState(isize::from(*checked));
                    item
                }
                MenuEntry::Sub(label, enabled, entries) => {
                    let text = chrome::mac_caption(i18n::tc("menu", label));
                    let (item, sub) = submenu(mtm, &text);
                    self.fill(&sub, entries);
                    item.setEnabled(*enabled);
                    item
                }
            };
            if divider {
                menu.addItem(&NSMenuItem::separatorItem(mtm));
                divider = false;
            }
            menu.addItem(&item);
        }
    }
}

fn submenu(mtm: MainThreadMarker, text: &str) -> (Retained<NSMenuItem>, Retained<NSMenu>) {
    let title = NSString::from_str(text);
    let item = NSMenuItem::new(mtm);
    let menu = NSMenu::initWithTitle(mtm.alloc(), &title);
    item.setTitle(&title);
    item.setSubmenu(Some(&menu));
    (item, menu)
}

pub(crate) struct MacMenu {
    app: Weak<RefCell<App>>,
    ctx: egui::Context,
    send: Sender<Cmd>,
    receive: Receiver<Cmd>,
    handlers: Vec<Retained<Handler>>,
    titles: Vec<Retained<NSMenuItem>>,
    settings: Option<Retained<NSMenuItem>>,
    lang: Option<&'static str>,
}

impl MacMenu {
    pub(crate) fn new(app: &Rc<RefCell<App>>, ctx: &egui::Context) -> Self {
        let (send, receive) = mpsc::channel();
        Self {
            app: Rc::downgrade(app),
            ctx: ctx.clone(),
            send,
            receive,
            handlers: vec![],
            titles: vec![],
            settings: None,
            lang: None,
        }
    }

    fn handler(
        &self,
        mtm: MainThreadMarker,
        section: Option<usize>,
        system: bool,
    ) -> Retained<Handler> {
        let this = Handler::alloc(mtm).set_ivars(State {
            app: self.app.clone(),
            ctx: self.ctx.clone(),
            send: self.send.clone(),
            section,
            system,
            commands: Default::default(),
            previous: Default::default(),
        });
        // SAFETY: NSObject's designated initializer on the just-allocated subclass.
        unsafe { msg_send![super(this), init] }
    }

    fn section(
        &self,
        mtm: MainThreadMarker,
        index: usize,
        label: &str,
        system: bool,
    ) -> (Retained<NSMenuItem>, Retained<Handler>) {
        let (item, sub) = submenu(mtm, &chrome::mac_caption(i18n::tc("menubar", label)));
        let handler = self.handler(mtm, Some(index), system);
        sub.setDelegate(Some(ProtocolObject::from_ref(&*handler)));
        // An empty submenu is disabled by AppKit before it asks its delegate for updates.
        handler.refresh(&sub);
        (item, handler)
    }

    pub(crate) fn tick(&mut self, app: &Rc<RefCell<App>>) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let ns = NSApplication::sharedApplication(mtm);
        if self.settings.is_none() {
            // Winit creates the application menu with Services, Hide and the system's ⌘Q.
            let Some(bar) = ns.mainMenu() else { return };
            let Some(application) = bar.itemAtIndex(0).and_then(|i| i.submenu()) else {
                return;
            };
            let handler = self.handler(mtm, None, true);
            let settings = handler.item(i18n::t("Settings..."), ",", Cmd::Options);
            application.insertItem_atIndex(&settings, 1.min(application.numberOfItems()));
            self.settings = Some(settings);
            self.handlers.push(handler);
            for (i, source) in chrome::bar_titles().into_iter().enumerate() {
                let (item, handler) = self.section(mtm, i, source, true);
                bar.addItem(&item);
                self.titles.push(item);
                self.handlers.push(handler);
            }
        }
        if self.lang != Some(i18n::code()) {
            self.lang = Some(i18n::code());
            for (item, source) in self.titles.iter().zip(chrome::bar_titles()) {
                let name = NSString::from_str(&chrome::mac_caption(i18n::tc("menubar", source)));
                item.setTitle(&name);
                if let Some(sub) = item.submenu() {
                    sub.setTitle(&name);
                }
            }
            if let Some(item) = &self.settings {
                item.setTitle(&NSString::from_str(i18n::t("Settings...")));
            }
        }
        for cmd in self.receive.try_iter() {
            app.borrow_mut().run(cmd, None, &self.ctx);
            self.ctx.request_ui_pass(egui::ViewportId::ROOT);
        }
    }

    pub(crate) fn toolbar_popup(&self, at: egui::Pos2) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(view) = super::macos_show_main_window().and_then(|w| w.contentView()) else {
            return;
        };
        let root = NSMenu::new(mtm);
        let mut handlers = Vec::new();
        for (i, title) in chrome::bar_titles().into_iter().enumerate() {
            let (item, handler) = self.section(mtm, i, title, false);
            root.addItem(&item);
            handlers.push(handler);
        }
        let zoom = f64::from(self.ctx.zoom_factor());
        let y = f64::from(at.y) * zoom;
        let y = if view.isFlipped() {
            y
        } else {
            view.bounds().size.height - y
        };
        root.popUpMenuPositioningItem_atLocation_inView(
            None,
            NSPoint::new(f64::from(at.x) * zoom, y),
            Some(&view),
        );
    }
}
