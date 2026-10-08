use crate::{
    App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    ObjectFit, Pixels, Style, StyleRefinement, Styled, Window,
};
#[cfg(target_os = "macos")]
use core_video::pixel_buffer::CVPixelBuffer;
use refineable::Refineable;

/// A source of a surface's content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SurfaceSource {
    /// A macOS image buffer from CoreVideo
    #[cfg(target_os = "macos")]
    Surface(CVPixelBuffer),
    /// A validated GPU-only Windows video buffer.
    #[cfg(all(target_os = "windows",feature = "native-video"))]
    D3dVideo(crate::native_video::D3dVideoFrame),
    #[cfg(all(target_os="linux",feature="native-video"))]
    /// An immutable Linux-native decoder frame backed by DMA-BUF planes.
    DmaVideo(crate::native_video::DmaVideoFrame),
}

#[cfg(target_os = "macos")]
impl From<CVPixelBuffer> for SurfaceSource {
    fn from(value: CVPixelBuffer) -> Self {
        SurfaceSource::Surface(value)
    }
}

#[cfg(all(target_os="windows",feature="native-video"))]
impl From<crate::native_video::D3dVideoFrame> for SurfaceSource {
    fn from(value:crate::native_video::D3dVideoFrame)->Self {Self::D3dVideo(value)}
}

#[cfg(all(target_os="linux",feature="native-video"))]
impl From<crate::native_video::DmaVideoFrame> for SurfaceSource {
    fn from(value:crate::native_video::DmaVideoFrame)->Self{Self::DmaVideo(value)}
}

/// A surface element.
pub struct Surface {
    source: SurfaceSource,
    object_fit: ObjectFit,
    style: StyleRefinement,
}

/// Create a new surface element.
pub fn surface(source: impl Into<SurfaceSource>) -> Surface {
    Surface {
        source: source.into(),
        object_fit: ObjectFit::Contain,
        style: Default::default(),
    }
}

impl Surface {
    /// Set the object fit for the image.
    pub fn object_fit(mut self, object_fit: ObjectFit) -> Self {
        self.object_fit = object_fit;
        self
    }
}

impl Element for Surface {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.refine(&self.style);
        let layout_id = window.request_layout(style, [], cx);
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        #[cfg_attr(not(target_os = "macos"), allow(unused_variables))] bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        #[cfg_attr(not(target_os = "macos"), allow(unused_variables))] window: &mut Window,
        _: &mut App,
    ) {
        match &self.source {
            #[cfg(target_os = "macos")]
            SurfaceSource::Surface(surface) => {
                let size = crate::size(surface.get_width().into(), surface.get_height().into());
                let new_bounds = self.object_fit.get_bounds(bounds, size);
                // TODO: Add support for corner_radii
                window.paint_surface(new_bounds, surface.clone());
            }
            #[cfg(all(target_os="windows",feature="native-video"))]
            SurfaceSource::D3dVideo(frame) => {
                use crate::native_video::{VideoFit,VideoRect};
                let fit=match &self.object_fit {ObjectFit::Fill=>VideoFit::Stretch,ObjectFit::Contain=>VideoFit::Contain,ObjectFit::Cover=>VideoFit::Cover,ObjectFit::ScaleDown=>VideoFit::ScaleDown,ObjectFit::None=>VideoFit::Native};
                let viewport=VideoRect{x:bounds.origin.x.0,y:bounds.origin.y.0,width:bounds.size.width.0,height:bounds.size.height.0};
                if let Some(rect)=frame.geometry().destination(viewport,fit) {
                    let output=crate::bounds(crate::point(crate::px(rect.x),crate::px(rect.y)),crate::size(crate::px(rect.width),crate::px(rect.height)));
                    window.with_content_mask(Some(crate::ContentMask{bounds}),|window|window.paint_native_video(output,frame.clone()));
                }
            }
            #[cfg(all(target_os="linux",feature="native-video"))]
            SurfaceSource::DmaVideo(frame) => {
                use crate::native_video::{VideoFit,VideoRect};
                let fit=match &self.object_fit {ObjectFit::Fill=>VideoFit::Stretch,ObjectFit::Contain=>VideoFit::Contain,ObjectFit::Cover=>VideoFit::Cover,ObjectFit::ScaleDown=>VideoFit::ScaleDown,ObjectFit::None=>VideoFit::Native};
                let viewport=VideoRect{x:bounds.origin.x.0,y:bounds.origin.y.0,width:bounds.size.width.0,height:bounds.size.height.0};
                if let Some(rect)=frame.geometry().destination(viewport,fit) {
                    let output=crate::bounds(crate::point(crate::px(rect.x),crate::px(rect.y)),crate::size(crate::px(rect.width),crate::px(rect.height)));
                    window.with_content_mask(Some(crate::ContentMask{bounds}),|window|window.paint_native_drm_video(output,frame.clone()));
                }
            }
            #[allow(unreachable_patterns)]
            _ => {}
        }
    }
}

impl IntoElement for Surface {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for Surface {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}
