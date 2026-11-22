//! The system status button receives both mouse and accessibility actions even while MDM
//! has no Dock tile. Its context menu shares the tray's existing commands and state.
#![allow(unsafe_code)]

use super::{Menu, Sender, TrayCmd};
use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSEventMask, NSEventModifierFlags, NSEventType, NSImage, NSMenu, NSStatusBar,
    NSStatusBarButton, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSData, NSObject, NSObjectProtocol, NSPoint, NSSize, NSString};
use tray_icon::menu::ContextMenu;

pub(super) struct State {
    menu: Menu,
    tx: Sender<TrayCmd>,
    ctx: egui::Context,
}

define_class!(
    // SAFETY: AppKit installs and invokes the target on the main thread.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MDMStatusButtonTarget"]
    #[ivars = State]
    struct Target;
    unsafe impl NSObjectProtocol for Target {}
    impl Target {
        #[unsafe(method(mdmStatusClick:))]
        fn click(&self, button: &NSStatusBarButton) {
            let event = NSApplication::sharedApplication(self.mtm()).currentEvent();
            if event.as_ref().is_some_and(|e| e.r#type() == NSEventType::RightMouseUp
                || e.r#type() == NSEventType::LeftMouseUp && e.modifierFlags().contains(NSEventModifierFlags::Control)) {
                // SAFETY: muda's Menu owns this NSMenu for the lifetime of our action target.
                let menu = unsafe { &*self.ivars().menu.ns_menu().cast::<NSMenu>() };
                let y = if button.isFlipped() { button.bounds().size.height } else { 0.0 };
                menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(0.0,y), Some(button));
            } else {
                crate::crash::asked();
                let cmd = if event.as_ref().is_some_and(|e| e.r#type() == NSEventType::LeftMouseUp && e.clickCount() == 2) {
                    TrayCmd::DoubleClick
                } else {
                    TrayCmd::Click(status_position(button))
                };
                let _ = self.ivars().tx.send(cmd);
                self.ivars().ctx.request_ui_pass(egui::ViewportId::ROOT);
            }
        }
    }
);

// The panel takes top-left screen pixels. Anchor it to the item even for AXPress.
fn status_position(button: &NSStatusBarButton) -> Option<(i32, i32)> {
    let window = button.window()?;
    let frame = window.frame();
    let primary = objc2_app_kit::NSScreen::screens(button.mtm()).firstObject()?;
    let scale = window.backingScaleFactor();
    super::click_at(
        (frame.origin.x + frame.size.width * 0.5) * scale,
        (primary.frame().size.height - frame.origin.y) * scale,
    )
}

pub(super) struct StatusItem {
    item: Retained<NSStatusItem>,
    target: Retained<Target>,
}

impl StatusItem {
    pub(super) fn new(menu: Menu, tx: Sender<TrayCmd>, ctx: &egui::Context) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let data = NSData::with_bytes(include_bytes!(
            "../../assets/icons/app/menu-bar-template.png"
        ));
        let image = NSImage::initWithData(mtm.alloc(), &data)?;
        image.setSize(NSSize::new(22.0, 22.0));
        image.setTemplate(true);
        let target = Target::alloc(mtm).set_ivars(State {
            menu,
            tx,
            ctx: ctx.clone(),
        });
        // SAFETY: NSObject initialization of the just-allocated subclass.
        let target: Retained<Target> = unsafe { msg_send![super(target), init] };
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        let status = Self { item, target };
        let button = status.item.button(mtm)?;
        button.setImage(Some(&image));
        button.setToolTip(Some(&NSString::from_str(crate::MAIN_TITLE)));
        button.sendActionOn(NSEventMask::LeftMouseUp | NSEventMask::RightMouseUp);
        // SAFETY: the status item owns its target for as long as AppKit can invoke it.
        unsafe {
            button.setTarget(Some(&*status.target));
            button.setAction(Some(sel!(mdmStatusClick:)));
        }
        Some(status)
    }

    pub(super) fn placed(&self) -> bool {
        self.item.isVisible()
            && self
                .item
                .button(self.target.mtm())
                .and_then(|b| b.window())
                .is_some()
    }

    pub(super) fn set_tooltip(&self, tip: &str) {
        if let Some(button) = self.item.button(self.target.mtm()) {
            button.setToolTip(Some(&NSString::from_str(tip)));
        }
    }
}

impl Drop for StatusItem {
    fn drop(&mut self) {
        NSStatusBar::systemStatusBar().removeStatusItem(&self.item);
    }
}
