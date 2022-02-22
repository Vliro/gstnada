use glib::prelude::*;
use crate::gst;
use crate::gst::Element;

mod imp;

// The public Rust wrapper type for our element
glib::wrapper! {
    pub struct Nadatx(ObjectSubclass<imp::Nadatx>) @extends gst::Element, gst::Object;
}

// GStreamer elements need to be thread-safe. For the private implementation this is automatically
// enforced but for the public wrapper type we need to specify this manually.
unsafe impl Send for Nadatx {}
unsafe impl Sync for Nadatx {}

// Registers the type for our element, and then registers in GStreamer under
// the name "nadatx" for being able to instantiate it via e.g.
// gst::ElementFactory::make().
pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    gst::Element::register(
        Some(plugin),
        "nadatx",
        gst::Rank::None,
        Nadatx::static_type(),
    )
}
