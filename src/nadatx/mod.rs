use glib::prelude::*;
use crate::gst;
use crate::gst::Element;

mod imp;
mod twcc_test;

// The public Rust wrapper type for our element
glib::wrapper! {
    pub struct nadatx(ObjectSubclass<imp::nadatx>) @extends gst::Element, gst::Object;
}

// GStreamer elements need to be thread-safe. For the private implementation this is automatically
// enforced but for the public wrapper type we need to specify this manually.
unsafe impl Send for nadatx {}
unsafe impl Sync for nadatx {}

// Registers the type for our element, and then registers in GStreamer under
// the name "nadatx" for being able to instantiate it via e.g.
// gst::ElementFactory::make().
pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    gst::Element::register(
        Some(plugin),
        "nadatx",
        gst::Rank::None,
        nadatx::static_type(),
    )
}
