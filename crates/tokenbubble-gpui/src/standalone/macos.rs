//! AppKit status item hosted by GPUI's main run loop. Actions cross a channel so
//! native callbacks never re-enter a GPUI entity update, including menu tracking.

use async_channel::Sender;
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained, sel,
};
use objc2_app_kit::{
    NSAccessibility, NSApplication, NSApplicationActivationPolicy, NSEventMask,
    NSEventModifierFlags, NSEventType, NSMenu, NSMenuItem, NSStatusBar, NSStatusBarButton,
    NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSNumber, NSObject, NSObjectProtocol, NSString};

#[derive(Clone, Copy, Debug)]
pub enum TrayEvent {
    Toggle,
    Dismiss,
    Refresh,
    Settings,
    About,
    Quit,
}

struct TargetIvars {
    events: Sender<TrayEvent>,
    menu: Retained<NSMenu>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. The target and every
    // AppKit object it owns are confined to GPUI's main thread.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TokenBubbleStatusTarget"]
    #[ivars = TargetIvars]
    struct StatusTarget;

    unsafe impl NSObjectProtocol for StatusTarget {}

    impl StatusTarget {
        #[unsafe(method(statusClicked:))]
        fn status_clicked(&self, button: &NSStatusBarButton) {
            let app = NSApplication::sharedApplication(self.mtm());
            if let Some(event) = app.currentEvent()
                && (event.r#type() == NSEventType::RightMouseUp
                    || event.modifierFlags().contains(NSEventModifierFlags::Control))
            {
                self.send(TrayEvent::Dismiss);
                NSMenu::popUpContextMenu_withEvent_forView(&self.ivars().menu, &event, button);
            } else {
                self.send(TrayEvent::Toggle);
            }
        }

        #[unsafe(method(menuSelected:))]
        fn menu_selected(&self, item: &NSMenuItem) {
            let event = match item.tag() {
                1 => TrayEvent::Refresh,
                2 => TrayEvent::Settings,
                3 => TrayEvent::About,
                4 => TrayEvent::Quit,
                _ => return,
            };
            self.send(event);
        }
    }
);

impl StatusTarget {
    fn send(&self, event: TrayEvent) {
        let _ = self.ivars().events.try_send(event);
    }

    fn new(events: Sender<TrayEvent>, mtm: MainThreadMarker) -> Retained<Self> {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("TokenBubble"));
        menu.setAutoenablesItems(false);
        let this = Self::alloc(mtm).set_ivars(TargetIvars { events, menu });
        // SAFETY: This is NSObject's designated initializer with the correct signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        for (title, tag) in [
            ("Refresh", 1),
            ("Settings…", 2),
            ("About TokenBubble", 3),
            ("Quit TokenBubble", 4),
        ] {
            if tag == 4 {
                this.ivars().menu.addItem(&NSMenuItem::separatorItem(mtm));
            }
            // SAFETY: The retained target implements menuSelected: and outlives the
            // menu's weak targets. The selector takes exactly one NSMenuItem.
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(title),
                    Some(sel!(menuSelected:)),
                    &NSString::from_str(""),
                )
            };
            item.setTag(tag);
            unsafe { item.setTarget(Some(&this)) };
            this.ivars().menu.addItem(&item);
        }
        this
    }
}

/// Owns both the status item and its action target. AppKit does not retain action targets.
pub struct Tray {
    item: Retained<NSStatusItem>,
    target: Retained<StatusTarget>,
    mtm: MainThreadMarker,
}

impl Tray {
    pub fn create(events: Sender<TrayEvent>) -> Result<Self, &'static str> {
        let mtm =
            MainThreadMarker::new().ok_or("status item must be created on the main thread")?;
        let app = NSApplication::sharedApplication(mtm);
        // Also applies when launched directly, rather than through the LSUIElement bundle.
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        let bar = NSStatusBar::systemStatusBar();
        let item = bar.statusItemWithLength(NSVariableStatusItemLength);
        let Some(button) = item.button(mtm) else {
            bar.removeStatusItem(&item);
            return Err("AppKit did not create a status button");
        };
        let target = StatusTarget::new(events, mtm);
        button.setTitle(&NSString::from_str("🫧"));
        button.setAccessibilityLabel(Some(&NSString::from_str("TokenBubble")));
        button.setAccessibilityHelp(Some(&NSString::from_str("TokenBubble")));
        button.setAccessibilityIdentifier(Some(&NSString::from_str("tokenbubble.status-item")));
        button.setToolTip(Some(&NSString::from_str("TokenBubble")));
        button.sendActionOn(NSEventMask::LeftMouseUp | NSEventMask::RightMouseUp);
        // SAFETY: StatusTarget is retained until after these weak references are cleared.
        unsafe {
            button.setTarget(Some(&target));
            button.setAction(Some(sel!(statusClicked:)));
        }
        Ok(Self { item, target, mtm })
    }

    pub fn set_tooltip(&self, tooltip: &str) {
        if let Some(button) = self.item.button(self.mtm) {
            button.setToolTip(Some(&NSString::from_str(tooltip)));
        }
    }

    /// Returns the display ID and a display-local top-left origin in logical points,
    /// matching GPUI's macOS WindowOptions (not global pixels or backing pixels).
    pub fn popup_origin(&self, width: f32, height: f32) -> Option<(u64, f32, f32)> {
        let button = self.item.button(self.mtm)?;
        let window = button.window()?;
        let screen = window.screen()?;
        let frame = screen.frame();
        let visible = screen.visibleFrame();
        let button_frame =
            window.convertRectToScreen(button.convertRect_toView(button.bounds(), None));
        let description = screen.deviceDescription();
        let screen_number = description.objectForKey(&NSString::from_str("NSScreenNumber"))?;
        let display = screen_number.downcast_ref::<NSNumber>()?.unsignedIntValue();

        let width = f64::from(width);
        let height = f64::from(height);
        let inset = 8.0;
        let min_x = visible.origin.x + inset;
        let max_x = (visible.origin.x + visible.size.width - width - inset).max(min_x);
        let x = (button_frame.origin.x + button_frame.size.width / 2.0 - width / 2.0)
            .clamp(min_x, max_x);
        // AppKit's origin is bottom-left in a global point space, whose secondary
        // screens may have negative x/y. Clamp there, then flip relative to THIS screen.
        let min_top = visible.origin.y + height + inset;
        let max_top = (visible.origin.y + visible.size.height - inset).max(min_top);
        let top = (button_frame.origin.y - inset).clamp(min_top, max_top);
        Some((
            u64::from(display),
            (x - frame.origin.x) as f32,
            (frame.origin.y + frame.size.height - top) as f32,
        ))
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        if let Some(button) = self.item.button(self.mtm) {
            // SAFETY: Clearing callbacks before releasing their retained target.
            unsafe {
                button.setAction(None);
                button.setTarget(None);
            }
        }
        for item in self.target.ivars().menu.itemArray() {
            unsafe { item.setTarget(None) };
        }
        NSStatusBar::systemStatusBar().removeStatusItem(&self.item);
    }
}
