use glib::prelude::*;
use crate::gst;
use crate::gst::Element;

mod imp;
#[cfg(test)]
mod twcc_test;

// The public Rust wrapper type for our element
glib::wrapper! {
    pub struct Gcctx(ObjectSubclass<imp::Gcctx>) @extends gst::Element, gst::Object;
}

// GStreamer elements need to be thread-safe. For the private implementation this is automatically
// enforced but for the public wrapper type we need to specify this manually.
unsafe impl Send for Gcctx {}
unsafe impl Sync for Gcctx {}

// Registers the type for our element, and then registers in GStreamer under
// the name "gcctx" for being able to instantiate it via e.g.
// gst::ElementFactory::make().
pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    gst::Element::register(
        Some(plugin),
        "gcctx",
        gst::Rank::None,
        Gcctx::static_type(),
    )
}
