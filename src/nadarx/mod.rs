use glib::prelude::*;
use crate::gst::Element;
use crate::gst;
mod imp;

mod nadarx;

// The public Rust wrapper type for our element
glib::wrapper! {
    pub struct Screamrx(ObjectSubclass<imp::Nadarx>) @extends gst::Element, gst::Object;
}

// GStreamer elements need to be thread-safe. For the private implementation this is automatically
// enforced but for the public wrapper type we need to specify this manually.
unsafe impl Send for Screamrx {}
unsafe impl Sync for Screamrx {}

// Registers the type for our element, and then registers in GStreamer under
// the name "rsscreamrx" for being able to instantiate it via e.g.
// gst::ElementFactory::make().
pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    gst::Element::register(
        Some(plugin),
        "nadarx",
        gst::Rank::None,
        Screamrx::static_type(),
    )
}
