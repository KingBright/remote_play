//! Explicit foreground-window control, with live WindowServer identity and hit guards.
//! Accessibility is used only for permission and app activation; unreliable AXWindows
//! data is not allowed to redirect a selected window to an application root object.
use core_foundation::{
    array::CFArray,
    base::{CFType, CFTypeRef, TCFType},
    boolean::CFBoolean,
    dictionary::{CFDictionary, CFDictionaryGetValue},
    number::CFNumber,
    string::CFString,
};
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
    fn AXUIElementCreateSystemWide() -> CFTypeRef;
    fn AXUIElementCopyElementAtPosition(
        element: CFTypeRef,
        x: f32,
        y: f32,
        result: *mut CFTypeRef,
    ) -> i32;
    fn AXUIElementGetPid(element: CFTypeRef, pid: *mut i32) -> i32;
    fn AXUIElementCopyAttributeValue(
        element: CFTypeRef,
        name: CFTypeRef,
        value: *mut CFTypeRef,
    ) -> i32;
    fn AXUIElementSetAttributeValue(element: CFTypeRef, name: CFTypeRef, value: CFTypeRef) -> i32;
    fn AXUIElementSetMessagingTimeout(element: CFTypeRef, timeout: f32) -> i32;
    fn CGWindowListCopyWindowInfo(options: u32, window: u32) -> CFTypeRef;
}
pub fn permission_available() -> bool {
    unsafe { AXIsProcessTrusted() }
}
fn attr(element: &CFType, name: &str) -> Option<CFType> {
    let name = CFString::new(name);
    let mut out = std::ptr::null();
    unsafe {
        if AXUIElementCopyAttributeValue(element.as_CFTypeRef(), name.as_CFTypeRef(), &mut out) != 0
            || out.is_null()
        {
            return None;
        }
        Some(CFType::wrap_under_create_rule(out))
    }
}
fn value(dictionary: &CFType, key: &str) -> Option<CFType> {
    let dictionary = dictionary.downcast::<CFDictionary>()?;
    let key = CFString::new(key);
    unsafe {
        let raw = CFDictionaryGetValue(dictionary.as_concrete_TypeRef(), key.as_CFTypeRef());
        (!raw.is_null()).then(|| CFType::wrap_under_get_rule(raw))
    }
}
fn number(d: &CFType, key: &str) -> Option<f64> {
    value(d, key)?.downcast::<CFNumber>()?.to_f64()
}
fn valid_bounds(r: CGRect) -> bool {
    [r.origin.x, r.origin.y, r.size.width, r.size.height]
        .iter()
        .all(|v| v.is_finite())
        && r.size.width >= 2.
        && r.size.height >= 2.
}
#[derive(Clone, Copy, Debug)]
struct WindowDescription {
    id: u32,
    pid: i32,
    bounds: CGRect,
    visible: bool,
    layer: i32,
    alpha: f64,
    unnamed: bool,
    generic_frame_caption: bool,
}
fn decode(d: CFType) -> Option<WindowDescription> {
    let b = value(&d, "kCGWindowBounds")?;
    let r = CGRect::new(
        &CGPoint::new(number(&b, "X")?, number(&b, "Y")?),
        &CGSize::new(number(&b, "Width")?, number(&b, "Height")?),
    );
    if !valid_bounds(r) {
        return None;
    }
    Some(WindowDescription {
        id: number(&d, "kCGWindowNumber")? as u32,
        pid: number(&d, "kCGWindowOwnerPID")? as i32,
        bounds: r,
        visible: value(&d, "kCGWindowIsOnscreen")
            .and_then(|v| v.downcast::<CFBoolean>())
            .is_some_and(|v| v.into()),
        layer: number(&d, "kCGWindowLayer")? as i32,
        alpha: number(&d, "kCGWindowAlpha").unwrap_or(1.),
        generic_frame_caption: value(&d, "kCGWindowName")
            .and_then(|v| v.downcast::<CFString>())
            .is_some_and(|s| s == "Window"),
        unnamed: value(&d, "kCGWindowName")
            .and_then(|v| v.downcast::<CFString>())
            .is_none_or(|v| v.to_string().is_empty()),
    })
}
fn windows(options: u32, id: u32) -> Result<Vec<WindowDescription>, String> {
    let raw = unsafe { CGWindowListCopyWindowInfo(options, id) };
    if raw.is_null() {
        return Err("WindowServer did not provide current window identities".into());
    }
    let list = unsafe { CFType::wrap_under_create_rule(raw) };
    let list = list
        .downcast::<CFArray>()
        .ok_or("Invalid WindowServer list")?;
    if list.len() > 4096 {
        return Err("Window list exceeds the bounded input check".into());
    }
    Ok(list
        .iter()
        .filter_map(|v| decode(unsafe { CFType::wrap_under_get_rule(*v) }))
        .collect())
}
fn current(id: u32, pid: i32) -> Result<WindowDescription, String> {
    let w = windows(1 << 3, id)?
        .into_iter()
        .find(|w| w.id == id)
        .ok_or("The selected application window has closed; input was not redirected")?;
    if w.pid != pid {
        return Err("The selected window changed process; reconnect the window".into());
    }
    if w.layer != 0 {
        return Err("The selected source is not a normal application window".into());
    }
    Ok(w)
}
fn contains(r: CGRect, p: CGPoint) -> bool {
    p.x >= r.origin.x
        && p.y >= r.origin.y
        && p.x < r.origin.x + r.size.width
        && p.y < r.origin.y + r.size.height
}
fn top_at(windows: &[WindowDescription], point: CGPoint) -> Option<u32> {
    windows
        .iter()
        .find(|w| w.visible && w.alpha > 0. && w.layer >= 0 && contains(w.bounds, point))
        .map(|w| w.id)
}
/// Native titlebar traffic-light surfaces can be separate layer-zero windows.
/// They are not document focus, but a same-process dialog still must block input.
/// This is deliberately narrower than discarding all small or unnamed windows.
fn observed_native_caption_geometry(child: &WindowDescription, parent: &WindowDescription) -> bool {
    // Some macOS releases name the traffic-light surface "Window". Never ignore
    // that caption alone: require the measured native size and top-left inset.
    // Pointer hit testing is unchanged; this normalization is only for focus.
    child.generic_frame_caption
        && (child.bounds.size.width - 66.).abs() <= 1.
        && (child.bounds.size.height - 20.).abs() <= 1.
        && (child.bounds.origin.x - parent.bounds.origin.x - 6.).abs() <= 1.
        && (child.bounds.origin.y - parent.bounds.origin.y - 6.).abs() <= 1.
}

fn frame_accessory(child: &WindowDescription, parent: &WindowDescription) -> bool {
    child.id != parent.id
        && child.pid == parent.pid
        && child.layer == 0
        && parent.layer == 0
        && (child.unnamed || observed_native_caption_geometry(child, parent))
        && parent.visible
        && parent.alpha > 0.
        && parent.bounds.size.width >= 160.
        && parent.bounds.size.height >= 80.
        && child.bounds.size.width <= 112.
        && child.bounds.size.height <= 36.
        && child.bounds.origin.x >= parent.bounds.origin.x
        && child.bounds.origin.y >= parent.bounds.origin.y
        && child.bounds.origin.x + child.bounds.size.width <= parent.bounds.origin.x + 120.
        && child.bounds.origin.y + child.bounds.size.height <= parent.bounds.origin.y + 40.
}
fn front_of_app(windows: &[WindowDescription], pid: i32) -> Option<u32> {
    windows
        .iter()
        .find(|w| {
            w.pid == pid
                && w.visible
                && w.layer >= 0
                && w.alpha > 0.
                && !windows.iter().any(|parent| frame_accessory(w, parent))
        })
        .map(|w| w.id)
}
pub struct WindowTarget {
    pub id: u32,
    pub pid: i32,
    application: CFType,
}
impl WindowTarget {
    pub fn new(id: u32, pid: i32) -> Result<Self, String> {
        if id == 0 || pid <= 0 {
            return Err("Invalid selected window identity".into());
        }
        if !permission_available() {
            return Err(
                "This signed RemotePlay app needs Accessibility permission on the host".into(),
            );
        }
        current(id, pid)?;
        let raw = unsafe { AXUIElementCreateApplication(pid) };
        if raw.is_null() {
            return Err("Application activation object unavailable".into());
        }
        let application = unsafe { CFType::wrap_under_create_rule(raw) };
        unsafe {
            AXUIElementSetMessagingTimeout(application.as_CFTypeRef(), 0.05);
        }
        Ok(Self {
            id,
            pid,
            application,
        })
    }
    pub fn bounds(&self) -> Result<CGRect, String> {
        let w = current(self.id, self.pid)?;
        if !w.visible || w.alpha <= 0. {
            return Err(
                "Restore the selected window on the host desktop before controlling it".into(),
            );
        }
        Ok(w.bounds)
    }
    fn app_frontmost(&self) -> bool {
        attr(&self.application, "AXFrontmost")
            .and_then(|v| v.downcast::<CFBoolean>())
            .is_some_and(|v| v.into())
    }
    pub fn focused(&self) -> bool {
        self.app_frontmost()
            && windows(1 | 16, 0).is_ok_and(|list| front_of_app(&list, self.pid) == Some(self.id))
    }
    pub fn focus_for_click(&self) -> Result<(), String> {
        self.bounds()?;
        if !self.app_frontmost() {
            let name = CFString::new("AXFrontmost");
            let yes = CFBoolean::true_value();
            let status = unsafe {
                AXUIElementSetAttributeValue(
                    self.application.as_CFTypeRef(),
                    name.as_CFTypeRef(),
                    yes.as_CFTypeRef(),
                )
            };
            if status != 0 {
                return Err(format!(
                    "The selected application could not be activated ({status}); input was not redirected"
                ));
            }
        }
        if self.app_frontmost() {
            Ok(())
        } else {
            Err("Wait for the selected application to become active, then click again".into())
        }
    }
    pub fn check_pointer(&self, x: u16, y: u16) -> Result<(), String> {
        let r = self.bounds()?;
        let p = crate::input_injector::normalized_pointer_location(x, y, r);
        // Same-process window ordering excludes the wrong document or sheet.
        // Cross-process hit testing below uses Accessibility, not the bounding box
        // of Dock's transparent full-screen Stage Manager surface.
        let owned: Vec<_> = windows(1 | 16, 0)?
            .into_iter()
            .filter(|w| w.pid == self.pid)
            .collect();
        if top_at(&owned, p) == Some(self.id) {
            // The global HID path is allowed only after the public hit-test agrees
            // the point belongs to the selected process. An AXApplication result
            // is sufficient here; a window title or synthetic AX child is not used.
            let raw = unsafe { AXUIElementCreateSystemWide() };
            if raw.is_null() {
                return Err("Cannot verify the foreground input target".into());
            }
            let system = unsafe { CFType::wrap_under_create_rule(raw) };
            unsafe {
                AXUIElementSetMessagingTimeout(system.as_CFTypeRef(), 0.05);
            }
            let mut raw_hit = std::ptr::null();
            let status = unsafe {
                AXUIElementCopyElementAtPosition(
                    system.as_CFTypeRef(),
                    p.x as f32,
                    p.y as f32,
                    &mut raw_hit,
                )
            };
            if status != 0 || raw_hit.is_null() {
                return Err(format!(
                    "Foreground hit test unavailable ({status}); no input sent"
                ));
            }
            let hit = unsafe { CFType::wrap_under_create_rule(raw_hit) };
            let mut pid = 0;
            if unsafe { AXUIElementGetPid(hit.as_CFTypeRef(), &mut pid) } != 0 || pid != self.pid {
                return Err("Another application covers the selected point; foreground window input is blocked".into());
            }
            Ok(())
        } else {
            Err("Another window covers the selected point. Bring the selected window forward; no input was sent to the covering window".into())
        }
    }
    pub fn alive(&self) -> bool {
        current(self.id, self.pid).is_ok()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn w(id: u32, pid: i32, x: f64) -> WindowDescription {
        WindowDescription {
            id,
            pid,
            bounds: CGRect::new(&CGPoint::new(x, 20.), &CGSize::new(300., 200.)),
            visible: true,
            layer: 0,
            alpha: 1.,
            unnamed: false,
            generic_frame_caption: false,
        }
    }
    #[test]
    fn native_titlebar_surface_is_not_mistaken_for_keyboard_focus() {
        let parent = w(1, 10, 150.);
        let mut controls = w(9, 10, 156.);
        controls.unnamed = true;
        controls.bounds = CGRect::new(&CGPoint::new(156., 26.), &CGSize::new(66., 20.));
        assert!(frame_accessory(&controls, &parent));
        assert_eq!(front_of_app(&[controls, parent], 10), Some(1));
    }
    #[test]
    fn observed_named_native_caption_does_not_steal_document_focus() {
        let parent = w(1, 10, 150.);
        let mut caption = w(9, 10, 156.);
        caption.generic_frame_caption = true;
        caption.bounds = CGRect::new(&CGPoint::new(156., 26.), &CGSize::new(66., 20.));
        assert!(frame_accessory(&caption, &parent));
        assert_eq!(front_of_app(&[caption, parent], 10), Some(1));
        // Do not normalize the hit-test path: clicking the actual accessory is not
        // permission to send arbitrary pointer input through it.
        assert_eq!(top_at(&[caption, parent], CGPoint::new(160., 30.)), Some(9));
    }
    #[test]
    fn generic_caption_is_not_a_blanket_focus_bypass() {
        let parent = w(1, 10, 150.);
        let mut child = w(9, 10, 156.);
        child.generic_frame_caption = true;
        child.bounds = CGRect::new(&CGPoint::new(156., 26.), &CGSize::new(66., 20.));
        for variation in 0..5 {
            let mut candidate = child;
            match variation {
                0 => candidate.bounds.origin.y += 50.,
                1 => candidate.bounds.size.width = 100.,
                2 => candidate.layer = 8,
                3 => candidate.pid = 20,
                _ => candidate.bounds.origin.x += 20.,
            }
            assert!(!frame_accessory(&candidate, &parent));
        }
        let modal = w(3, 10, 170.);
        assert_eq!(front_of_app(&[modal, child, parent], 10), Some(3));
    }

    #[test]
    fn small_content_window_or_modal_is_not_ignored() {
        let parent = w(1, 10, 150.);
        let mut popup = w(2, 10, 160.);
        popup.unnamed = true;
        popup.bounds = CGRect::new(&CGPoint::new(160., 100.), &CGSize::new(66., 20.));
        assert!(!frame_accessory(&popup, &parent));
        assert_eq!(front_of_app(&[popup, parent], 10), Some(2));
        popup.bounds.origin.y = 26.;
        popup.layer = 8;
        assert_eq!(front_of_app(&[popup, parent], 10), Some(2));
    }
    #[test]
    fn named_window_in_titlebar_area_is_not_ignored() {
        let parent = w(1, 10, 150.);
        let mut popup = w(2, 10, 156.);
        popup.bounds = CGRect::new(&CGPoint::new(156., 26.), &CGSize::new(66., 20.));
        assert!(!frame_accessory(&popup, &parent));
    }
    #[test]
    fn topmost_overlap_never_selects_background_window() {
        let list = [w(2, 10, 0.), w(1, 10, 0.)];
        assert_eq!(top_at(&list, CGPoint::new(50., 60.)), Some(2));
        assert_eq!(front_of_app(&list, 10), Some(2));
    }
    #[test]
    fn other_application_occlusion_is_respected() {
        let list = [w(2, 20, 0.), w(1, 10, 0.)];
        assert_eq!(top_at(&list, CGPoint::new(50., 60.)), Some(2));
    }
    #[test]
    fn higher_layer_same_process_modal_blocks_keyboard_target() {
        let mut modal = w(2, 10, 0.);
        modal.layer = 8;
        assert_eq!(front_of_app(&[modal, w(1, 10, 0.)], 10), Some(2));
    }
    #[test]
    fn same_process_filter_still_requires_separate_cross_process_hit_test() {
        let mut overlay = w(99, 20, 0.);
        overlay.layer = 20;
        let list = [overlay, w(1, 10, 0.)];
        let owned: Vec<_> = list.into_iter().filter(|w| w.pid == 10).collect();
        assert_eq!(top_at(&owned, CGPoint::new(50., 60.)), Some(1));
    }
    #[test]
    fn nonoverlap_selects_correct_same_process_window() {
        let list = [w(2, 10, 400.), w(1, 10, 0.)];
        assert_eq!(top_at(&list, CGPoint::new(50., 60.)), Some(1));
    }
    #[test]
    fn hidden_windows_cannot_receive_input() {
        let mut hidden = w(2, 10, 0.);
        hidden.visible = false;
        assert_eq!(
            top_at(&[hidden, w(1, 10, 0.)], CGPoint::new(50., 60.)),
            Some(1)
        );
    }
    #[test]
    fn negative_origins_are_valid_but_right_bottom_borders_excluded() {
        let r = w(1, 10, -400.).bounds;
        assert!(valid_bounds(r));
        assert!(contains(r, CGPoint::new(-300., 50.)));
        assert!(!contains(r, CGPoint::new(-100., 50.)));
    }
    #[test]
    fn nonfinite_geometry_rejected() {
        assert!(!valid_bounds(CGRect::new(
            &CGPoint::new(f64::NAN, 0.),
            &CGSize::new(100., 100.)
        )));
    }
}
