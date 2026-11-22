//! Track form choices with AppKit between egui frames, outside the owning dialog's borrow.
#![allow(unsafe_code)]

use egui::{Context, Id, Rect, ViewportId};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSString};
use std::cell::Cell;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MDMFormChoiceTarget"]
    #[ivars = Cell<Option<usize>>]
    struct Target;
    unsafe impl NSObjectProtocol for Target {}
    impl Target {
        #[unsafe(method(mdmChooseValue:))]
        fn choose(&self, sender: &NSMenuItem) {
            self.ivars().set(usize::try_from(sender.tag()).ok());
        }
    }
);

#[derive(Clone)]
struct Request {
    reply: Id,
    viewport: ViewportId,
    window: isize,
    anchor: Rect,
    rtl: bool,
    labels: Vec<String>,
    selected: Option<usize>,
    empty: String,
}

fn pending() -> Id {
    Id::unique("mac-form-choice-request")
}

pub(crate) fn choose(
    ctx: &Context,
    id: Id,
    anchor: Rect,
    labels: impl Iterator<Item = impl Into<String>>,
    selected: Option<usize>,
    empty: &str,
    open: &mut bool,
) -> Option<usize> {
    let viewport = ctx.viewport_id();
    let generation = crate::dialogs::opening_key(ctx);
    let reply = ctx.data(|d| id.with((viewport, d.get_temp::<u64>(generation))));
    if let Some(result) = ctx.data_mut(|d| d.remove_temp::<Option<usize>>(reply)) {
        *open = false;
        return result;
    }
    // Only one menu can track the pointer. Multiple egui passes can ask before
    // the root viewport gets to present it.
    if let Some(owner) = ctx.data(|d| d.get_temp::<Id>(pending())) {
        *open = owner == reply;
        return None;
    }
    let window =
        MainThreadMarker::new().and_then(|m| NSApplication::sharedApplication(m).keyWindow());
    let Some(window) = window else {
        *open = false;
        return None;
    };
    let request = Request {
        reply,
        viewport,
        window: window.windowNumber(),
        anchor,
        rtl: crate::winui::rtl(),
        labels: labels.map(Into::into).collect(),
        selected,
        empty: empty.to_owned(),
    };
    ctx.data_mut(|d| {
        d.insert_temp(pending(), reply);
        d.insert_temp(pending(), Some(request));
    });
    ctx.request_repaint_of(ViewportId::ROOT);
    None
}

pub(super) fn track(ctx: &Context, ns: &NSApplication) {
    let req = ctx.data_mut(|d| d.remove_temp::<Option<Request>>(pending()).flatten());
    let Some(req) = req else { return };
    let result = ns
        .keyWindow()
        .filter(|w| w.windowNumber() == req.window)
        .and_then(|w| w.contentView())
        .and_then(|view| {
            let mtm = ns.mtm();
            let menu = NSMenu::new(mtm);
            menu.setAutoenablesItems(false);
            let target = Target::alloc(mtm).set_ivars(Cell::new(None));
            // SAFETY: NSObject's designated initializer on the just-allocated subclass.
            let target: objc2::rc::Retained<Target> = unsafe { msg_send![super(target), init] };
            for (i, label) in req.labels.iter().enumerate() {
                // SAFETY: target implements this selector and lives through menu tracking.
                let item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        mtm.alloc(),
                        &NSString::from_str(label),
                        Some(sel!(mdmChooseValue:)),
                        &NSString::new(),
                    )
                };
                unsafe { item.setTarget(Some(&*target)) };
                item.setTag(i as isize);
                item.setState(isize::from(Some(i) == req.selected));
                menu.addItem(&item);
            }
            if req.labels.is_empty() {
                let item = NSMenuItem::new(mtm);
                item.setTitle(&NSString::from_str(&req.empty));
                item.setEnabled(false);
                menu.addItem(&item);
            }
            let zoom = f64::from(ctx.zoom_factor());
            menu.setMinimumWidth(f64::from(req.anchor.width()) * zoom);
            let bounds = view.bounds();
            let x = if req.rtl {
                f64::from(req.anchor.right()) * zoom - menu.size().width
            } else {
                f64::from(req.anchor.left()) * zoom
            };
            let selected = req
                .selected
                .filter(|&i| i < req.labels.len())
                .and_then(|i| menu.itemAtIndex(i as isize));
            let y = f64::from(if selected.is_some() {
                req.anchor.top()
            } else {
                req.anchor.bottom()
            }) * zoom;
            let at = NSPoint::new(
                bounds.origin.x + x,
                bounds.origin.y
                    + if view.isFlipped() {
                        y
                    } else {
                        bounds.size.height - y
                    },
            );
            menu.popUpMenuPositioningItem_atLocation_inView(selected.as_deref(), at, Some(&view));
            target.ivars().get()
        });
    ctx.data_mut(|d| {
        d.remove::<Id>(pending());
        d.insert_temp(req.reply, result);
    });
    ctx.request_repaint_of(req.viewport);
}
