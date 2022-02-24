use glib::prelude::*;
use crate::gst::Element;
use crate::gst;
mod imp;

mod gccrx;

// The public Rust wrapper type for our element
glib::wrapper! {
    pub struct Gccrx(ObjectSubclass<imp::Gccrx>) @extends gst::Element, gst::Object;
}

// GStreamer elements need to be thread-safe. For the private implementation this is automatically
// enforced but for the public wrapper type we need to specify this manually.
unsafe impl Send for Gccrx {}
unsafe impl Sync for Gccrx {}

// Registers the type for our element, and then registers in GStreamer under
// the name "rsscreamrx" for being able to instantiate it via e.g.
// gst::ElementFactory::make().
pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    gst::Element::register(
        Some(plugin),
        "gccrx",
        gst::Rank::None,
        Gccrx::static_type(),
    )
}

pub (crate) unsafe fn cast_to_raw_pointer<T>(val: &T) -> *const u8 {
    val as *const T as usize as *const u8
}

pub (crate) unsafe fn cast_from_raw_pointer<T>(val: *const u8) -> *const T {
    val as usize as *const T
}