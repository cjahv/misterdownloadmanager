//! AppKit reads the current command tree between frames, when a menu opens.
//! Selections go back through the application's regular command dispatcher.
#![allow(unsafe_code)]

use crate::app::{App, Cmd};
use crate::chrome::MenuEntry;
use crate::{chrome, i18n};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSMenu, NSMenuDelegate, NSMenuItem, NSRunningApplication};
use objc2_foundation::{
    NSAppleEventDescriptor, NSAppleEventManager, NSObject, NSObjectProtocol, NSPoint, NSString,
};
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::mpsc::{self, Receiver, Sender};

const CORE_EVENT: u32 = u32::from_be_bytes(*b"aevt");
const QUIT_EVENT: u32 = u32::from_be_bytes(*b"quit");

type MenuSnapshot = (&'static str, Vec<MenuEntry<Cmd>>);

pub(crate) struct PopupMenu {
    pub at: egui::Pos2,
    pub entries: Vec<MenuEntry<Cmd>>,
    pub target: Option<u64>,
}

pub struct State {
    app: Weak<RefCell<App>>,
    ctx: egui::Context,
    send: Sender<(Cmd, Option<u64>)>,
    section: Option<usize>,
    target: Option<u64>,
    commands: RefCell<Vec<Cmd>>,
    previous: RefCell<Option<MenuSnapshot>>,
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
        #[unsafe(method(mdmQuitEvent:reply:))]
        fn quit_event(&self, event: &NSAppleEventDescriptor, reply: &NSAppleEventDescriptor) {
            // keySenderPIDAttr is supplied by the system. Only the Dock's interactive Quit
            // becomes Hide. Let AppKit finish logouts, updates and other termination requests.
            let pid: Option<Retained<NSAppleEventDescriptor>> = unsafe {
                msg_send![event, attributeDescriptorForKeyword: u32::from_be_bytes(*b"spid")]
            };
            let sender = pid.and_then(|pid| NSRunningApplication::runningApplicationWithProcessIdentifier(pid.int32Value()))
                .and_then(|app| app.bundleIdentifier());
            let reason: Option<Retained<NSAppleEventDescriptor>> = unsafe {
                msg_send![event, paramDescriptorForKeyword: u32::from_be_bytes(*b"why?")]
            };
            if reason.is_none() && sender.is_some_and(|id| id.isEqualToString(objc2_foundation::ns_string!("com.apple.dock"))) {
                self.dispatch(Cmd::HideApplication);
                // Report userCanceledErr to the requester: the process is intentionally alive.
                // typeNull means the Dock did not request a reply.
                let kind: u32 = unsafe { msg_send![reply, descriptorType] };
                if kind != u32::from_be_bytes(*b"null") {
                    let cancelled = NSAppleEventDescriptor::descriptorWithInt32(-128);
                    unsafe {
                        let _: () = msg_send![reply, setParamDescriptor: &*cancelled, forKeyword: u32::from_be_bytes(*b"errn")];
                    }
                }
            } else {
                NSApplication::sharedApplication(self.mtm()).terminate(None);
            }
        }

        #[unsafe(method(mdmChoose:))]
        fn choose(&self, sender: &NSMenuItem) {
            let state = self.ivars();
            if let Some(cmd) = state.commands.borrow().get(sender.tag() as usize) {
                self.dispatch(*cmd);
            }
        }
    }
);

impl Handler {
    fn dispatch(&self, cmd: Cmd) {
        let state = self.ivars();
        let _ = state.send.send((cmd, state.target));
        state.ctx.request_ui_pass(egui::ViewportId::ROOT);
    }

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
        if section == 1 {
            // With a nil target AppKit sends performClose: to the key window, just like its
            // red close button. This also preserves the main window's close-to-tray policy.
            let close = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    self.mtm().alloc(),
                    &NSString::from_str(i18n::t("Close")),
                    Some(sel!(performClose:)),
                    &NSString::from_str("w"),
                )
            };
            menu.addItem(&NSMenuItem::separatorItem(self.mtm()));
            menu.addItem(&close);
        }
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
        // Delay a separator so moving Settings to the app menu leaves no trailing rule.
        let mut divider = false;
        for entry in entries {
            let item = match entry {
                MenuEntry::Sep => {
                    divider = menu.numberOfItems() > 0;
                    continue;
                }
                MenuEntry::Item(_, Cmd::Options, ..) if self.ivars().section.is_some() => {
                    continue;
                }
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
    send: Sender<(Cmd, Option<u64>)>,
    receive: Receiver<(Cmd, Option<u64>)>,
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
        target: Option<u64>,
    ) -> Retained<Handler> {
        let this = Handler::alloc(mtm).set_ivars(State {
            app: self.app.clone(),
            ctx: self.ctx.clone(),
            send: self.send.clone(),
            section,
            target,
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
    ) -> (Retained<NSMenuItem>, Retained<Handler>) {
        let (item, sub) = submenu(mtm, &chrome::mac_caption(i18n::tc("menubar", label)));
        let handler = self.handler(mtm, Some(index), None);
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
            let handler = self.handler(mtm, None, None);
            // The app menu and its shortcuts use the same hide path as the Dock's Quit.
            // The status item's Exit is a separate tray command and still calls App::quit.
            for item in application.itemArray().iter().filter(|item| {
                matches!(item.action(), Some(action) if action == sel!(hide:) || action == sel!(terminate:))
            }) {
                let mut commands = handler.ivars().commands.borrow_mut();
                item.setTag(commands.len() as isize);
                commands.push(Cmd::HideApplication);
                // SAFETY: handler implements the selector and lives with the application's menu.
                unsafe {
                    item.setAction(Some(sel!(mdmChoose:)));
                    item.setTarget(Some(&*handler));
                }
            }
            // SAFETY: the two-argument Apple event selector is implemented above. Drop
            // unregisters it before releasing the handler.
            unsafe {
                let _: () = msg_send![&*NSAppleEventManager::sharedAppleEventManager(),
                    setEventHandler: &*handler, andSelector: sel!(mdmQuitEvent:reply:),
                    forEventClass: CORE_EVENT, andEventID: QUIT_EVENT];
            }
            let settings = handler.item(i18n::t("Settings..."), ",", Cmd::Options);
            application.insertItem_atIndex(&settings, 1.min(application.numberOfItems()));
            self.settings = Some(settings);
            self.handlers.push(handler);
            for (i, source) in chrome::bar_titles().into_iter().enumerate() {
                let (item, handler) = self.section(mtm, i, source);
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
        super::macos_choices::track(&self.ctx, &ns);
        // Release the application borrow before AppKit starts its tracking loop.
        let popup = app.borrow_mut().take_native_popup();
        if let Some(PopupMenu {
            at,
            entries,
            target,
        }) = popup
            && let Some(view) = super::macos_main_window(&ns).and_then(|w| w.contentView())
        {
            let handler = self.handler(mtm, None, target);
            let menu = NSMenu::new(mtm);
            handler.fill(&menu, &entries);
            let zoom = f64::from(self.ctx.zoom_factor());
            let bounds = view.bounds();
            let y = f64::from(at.y) * zoom;
            let point = NSPoint::new(
                bounds.origin.x + f64::from(at.x) * zoom,
                bounds.origin.y
                    + if view.isFlipped() {
                        y
                    } else {
                        bounds.size.height - y
                    },
            );
            menu.popUpMenuPositioningItem_atLocation_inView(None, point, Some(&view));
            self.ctx.request_repaint();
        }
        for (cmd, target) in self.receive.try_iter() {
            app.borrow_mut().run(cmd, target, &self.ctx);
            self.ctx.request_ui_pass(egui::ViewportId::ROOT);
        }
    }
}

impl Drop for MacMenu {
    fn drop(&mut self) {
        if self.settings.is_some() {
            // SAFETY: remove the exact class and event installed in tick.
            unsafe {
                let _: () = msg_send![&*NSAppleEventManager::sharedAppleEventManager(),
                    removeEventHandlerForEventClass: CORE_EVENT, andEventID: QUIT_EVENT];
            }
        }
    }
}
