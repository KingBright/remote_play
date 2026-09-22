use protocol::session::{CaptureSource, CaptureSourceInfo};
use std::error::Error;

#[cfg(target_os = "macos")]
fn window_pixel_size(
    window: &screencapturekit::shareable_content::SCWindow,
    filter: &screencapturekit::prelude::SCContentFilter,
) -> (u32, u32) {
    // Stage Manager reports transformed thumbnail bounds in both SCWindow.frame
    // and SCContentFilter.contentRect. AXSize retains the original window size.
    // Only use a unique title match; never choose another same-titled window.
    let original = window.owning_application().and_then(|app| {
        window
            .title()
            .and_then(|title| accessible_window_size(app.process_id(), &title))
    });
    let rect = filter.content_rect();
    let size = original.unwrap_or_else(|| {
        let rect = if rect.size().width >= 2.0 {
            rect
        } else {
            window.frame()
        };
        (rect.size().width, rect.size().height)
    });
    let scale = f64::from(filter.point_pixel_scale()).clamp(1.0, 4.0);
    ((size.0 * scale) as u32, (size.1 * scale) as u32)
}

#[cfg(target_os = "macos")]
fn accessible_window_size(pid: i32, title: &str) -> Option<(f64, f64)> {
    use core_foundation::{
        array::CFArray,
        base::{CFType, CFTypeRef, TCFType},
        string::CFString,
    };
    use core_graphics::geometry::CGSize;
    use std::ffi::c_void;
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
        fn AXUIElementCopyAttributeValue(
            element: CFTypeRef,
            attribute: CFTypeRef,
            value: *mut CFTypeRef,
        ) -> i32;
        fn AXUIElementSetMessagingTimeout(element: CFTypeRef, timeout: f32) -> i32;
        fn AXValueGetType(value: CFTypeRef) -> u32;
        fn AXValueGetValue(value: CFTypeRef, kind: u32, output: *mut c_void) -> bool;
    }
    fn attribute(element: CFTypeRef, name: &str) -> Option<CFType> {
        let mut value = std::ptr::null();
        let name = CFString::new(name);
        // Copy follows the CF ownership rule; errors (including no permission)
        // simply leave capture on ScreenCaptureKit's geometry.
        unsafe {
            if AXUIElementCopyAttributeValue(element, name.as_CFTypeRef(), &mut value) != 0
                || value.is_null()
            {
                return None;
            }
            Some(CFType::wrap_under_create_rule(value))
        }
    }
    unsafe {
        let raw = AXUIElementCreateApplication(pid);
        if raw.is_null() {
            return None;
        }
        let app = CFType::wrap_under_create_rule(raw);
        AXUIElementSetMessagingTimeout(app.as_CFTypeRef(), 0.05);
        let windows = attribute(app.as_CFTypeRef(), "AXWindows")?.downcast::<CFArray>()?;
        let mut matched = None;
        for window in windows.iter() {
            let Some(name) =
                attribute(*window, "AXTitle").and_then(|value| value.downcast::<CFString>())
            else {
                continue;
            };
            if name != title {
                continue;
            }
            if matched.is_some() {
                return None;
            }
            let value = attribute(*window, "AXSize")?;
            if AXValueGetType(value.as_CFTypeRef()) != 2 {
                return None;
            }
            let mut size = CGSize::new(0.0, 0.0);
            if !AXValueGetValue(value.as_CFTypeRef(), 2, (&mut size as *mut CGSize).cast()) {
                return None;
            }
            if size.width < 2.0 || size.height < 2.0 {
                return None;
            }
            matched = Some((size.width, size.height));
        }
        matched
    }
}

pub fn list() -> Result<Vec<CaptureSourceInfo>, Box<dyn Error + Send + Sync>> {
    #[cfg(target_os = "macos")]
    {
        use screencapturekit::prelude::*;
        let content = SCShareableContent::get()?;
        let mut sources = Vec::new();
        for display in content.displays() {
            sources.push(CaptureSourceInfo {
                source: CaptureSource::Display(display.display_id()),
                title: format!("Display {}", display.display_id()),
                application: String::new(),
                process_id: None,
                width: display.width(),
                height: display.height(),
                supports_input: display.display_id()
                    == core_graphics::display::CGDisplay::main().id,
            });
        }
        for window in content.windows() {
            let Some(app) = window.owning_application() else {
                continue;
            };
            if app.process_id() == std::process::id() as i32 {
                continue;
            }
            let Some(title) = window.title().filter(|title| !title.trim().is_empty()) else {
                continue;
            };
            let frame = window.frame();
            if frame.size().width < 2.0 || frame.size().height < 2.0 {
                continue;
            }
            let independent = SCContentFilter::create().with_window(&window).build();
            let size = window_pixel_size(&window, &independent);
            sources.push(CaptureSourceInfo {
                source: CaptureSource::Window(window.window_id()),
                title,
                application: app.application_name(),
                process_id: Some(app.process_id()),
                width: size.0,
                height: size.1,
                // Screen capture permission never implies window-scoped input injection.
                supports_input: false,
            });
        }
        Ok(sources)
    }
    #[cfg(not(target_os = "macos"))]
    Ok(vec![CaptureSourceInfo {
        source: CaptureSource::MainDisplay,
        title: "Desktop".into(),
        application: String::new(),
        process_id: None,
        width: 0,
        height: 0,
        supports_input: true,
    }])
}

#[cfg(target_os = "macos")]
pub(crate) fn filter(
    source: CaptureSource,
) -> Result<(screencapturekit::prelude::SCContentFilter, (u32, u32)), Box<dyn Error + Send + Sync>>
{
    use screencapturekit::prelude::*;
    let content = SCShareableContent::get()?;
    match source {
        CaptureSource::Window(id) => {
            let window = content
                .windows()
                .into_iter()
                .find(|window| window.window_id() == id)
                .ok_or("selected window is no longer available")?;
            let filter = SCContentFilter::create().with_window(&window).build();
            let size = window_pixel_size(&window, &filter);
            Ok((filter, size))
        }
        CaptureSource::MainDisplay | CaptureSource::Display(_) => {
            let id = match source {
                CaptureSource::Display(id) => id,
                _ => core_graphics::display::CGDisplay::main().id,
            };
            let display = content
                .displays()
                .into_iter()
                .find(|display| display.display_id() == id)
                .ok_or("selected display is no longer available")?;
            Ok((
                SCContentFilter::create().with_display(&display).build(),
                (display.width(), display.height()),
            ))
        }
    }
}
