use glib::prelude::*;
use glib::subclass::{Signal, SignalType};
use glib::subclass::prelude::*;

use gst::prelude::*;
use gst::subclass::prelude::*;

use std::convert::TryInto;
use std::ffi::CString;
use std::sync::atomic::AtomicPtr;
use std::{thread, time};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use bitreader::BitReader;
use gstreamer::{Bin, debug, error, info, log, trace};

use gstreamer_video as gstv;


pub use gstreamer_rtp::rtp_buffer::compare_seqnum;
pub use gstreamer_rtp::rtp_buffer::RTPBuffer;
pub use gstreamer_rtp::rtp_buffer::RTPBufferExt;
use hashbrown::HashMap;
use core::sync::atomic::Ordering::Relaxed;
use gstreamer_sys::GstBin;
use once_cell::sync::Lazy;
use crate::gccrx::{cast_from_raw_pointer, cast_to_raw_pointer};
use crate::gst;

const DEFAULT_CURRENT_MAX_BITRATE: u32 = 0;

#[repr(C)]
#[derive(Copy, Clone)]
pub(crate) struct RazorController(*const std::ffi::c_void);

unsafe impl Sync for RazorController {}

unsafe impl Send for RazorController {}

// Property value storage
#[derive(Debug, Clone)]
struct Settings {
    params: Option<String>,
    current_max_bitrate: u32,
}

struct Data {
    pub gx: *const Gcctx,
    pub ctrl: Mutex<RazorController>
}
unsafe impl Send for Data {}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            params: None,
            current_max_bitrate: DEFAULT_CURRENT_MAX_BITRATE,
        }
    }
}

struct ClockWait {
    clock_id: Option<gst::ClockId>,
    _flushing: bool,
}

// static STREAMTX_PTR: Option<&Gcctx>  = None;

// Struct containing all the element data
#[repr(C)]
pub struct Gcctx {
    srcpad: gst::Pad,
    sinkpad: gst::Pad,
    rtcp_srcpad: gst::Pad,
    rtcp_sinkpad: gst::Pad,
    settings: Mutex<Settings>,
    data: Mutex<HashMap<u32, gst::Buffer>>,
    controller: Mutex<RazorController>,
    clock_wait: Mutex<ClockWait>,
}

static CAT: Lazy<gst::DebugCategory> = Lazy::new(|| {
    gst::DebugCategory::new(
        "Gcctx",
        gst::DebugColorFlags::empty(),
        Some("Gcctx Element"),
    )
});


impl Drop for Gcctx {
    fn drop(&mut self) {
        unsafe {
            self.with_controller(|c| sender_cc_destroy(c));
        }
    }
}

#[derive(Debug)]
pub(crate) struct Feedback {
    ecn: u8,
    seq: u16,
    ts: u64,
}

//        uint16_t sequence;
//         uint64_t rxTimestampUs;
//         uint8_t ecn;
impl Feedback {
    pub fn new(ecn: u8, seq: u16) -> Feedback {
        Feedback {
            ecn,
            seq,
            ts: 0,
        }
    }
}

impl Gcctx {

    fn with_controller(&self, f : impl FnOnce(RazorController)) {
        let c = self.controller.lock().unwrap();
        f(*c);
        drop(c);
    }
    // Called whenever a new buffer is passed to our sink pad. Here buffers should be processed and
    // whenever some output buffer is available have to push it out of the source pad.
    // Here we just pass through all buffers directly
    //
    // See the documentation of gst::Buffer and gst::BufferRef to see what can be done with
    // buffers.
    fn sink_chain(
        &self,
        pad: &gst::Pad,
        element: &super::Gcctx,
        buffer: gst::Buffer,
    ) -> Result<gst::FlowSuccess, gst::FlowError> {
        // trace!(CAT, obj: pad, "gstnada Handling buffer {:?}", buffer);
        let rtp_buffer = RTPBuffer::from_buffer_readable(&buffer).unwrap();
        let seq = rtp_buffer.seq();
        let payload_type = rtp_buffer.payload_type();
        let timestamp = rtp_buffer.timestamp();
        let _ssrc = rtp_buffer.ssrc();
        let _marker = rtp_buffer.is_marker() as u8;
        //println!("sink chain");
        trace!(
            CAT,
            obj: pad,
            "gstnada Handling rtp buffer seq {} payload_type {} timestamp {} ",
            seq,
            payload_type,
            timestamp
        );
        drop(rtp_buffer);
        let size = buffer.size().try_into().unwrap();
        let mut force_idr: u32 = 1;
        {
            self.data.lock().unwrap().insert(seq as u32, buffer);
        }
        unsafe {
            self.with_controller(|c|  sender_cc_add_pace_packet(c, seq as u32, 0, size));
        }

        if force_idr != 0 {
            let event = gstv::UpstreamForceKeyUnitEvent::builder()
                .all_headers(true)
                .build();
            let rc = self.sinkpad.push_event(event);
            //println!("imp.rs: force_idr rc {} enabled 1 ", rc);
        }
        glib::bitflags::_core::result::Result::Ok(gst::FlowSuccess::Ok)
    }

    // Called whenever an event arrives on the sink pad. It has to be handled accordingly and in
    // most cases has to be either passed to Pad::event_default() on this pad for default handling,
    // or Pad::push_event() on all pads with the opposite direction for direct forwarding.
    // Here we just pass through all events directly to the source pad.
    //
    // See the documentation of gst::Event and gst::EventRef to see what can be done with
    // events, and especially the gst::EventView type for inspecting events.
    fn sink_event(&self, pad: &gst::Pad, _element: &super::Gcctx, event: gst::Event) -> bool {
        log!(
            CAT,
            obj: pad,
            "gstnada Handling event {:?} {:?}",
            event,
            event.type_()
        );
        // println!("gstnada Handling sink event {:?}", event);
        self.srcpad.push_event(event)
    }

    // Called whenever a query is sent to the sink pad. It has to be answered if the element can
    // handle it, potentially by forwarding the query first to the peer pads of the pads with the
    // opposite direction, or false has to be returned. Default handling can be achieved with
    // Pad::query_default() on this pad and forwarding with Pad::peer_query() on the pads with the
    // opposite direction.
    // Here we just forward all queries directly to the source pad's peers.
    //
    // See the documentation of gst::Query and gst::QueryRef to see what can be done with
    // queries, and especially the gst::QueryView type for inspecting and modifying queries.
    fn sink_query(
        &self,
        pad: &gst::Pad,
        _element: &super::Gcctx,
        query: &mut gst::QueryRef,
    ) -> bool {
        log!(CAT, obj: pad, "gstnada Handling query {:?}", query);
        self.srcpad.peer_query(query)
    }

    fn rtcp_sink_chain(
        &self,
        pad: &gst::Pad,
        element: &super::Gcctx,
        buffer: gst::Buffer,
    ) -> Result<gst::FlowSuccess, gst::FlowError> {
        // trace!(CAT, obj: pad, "gstnada Handling buffer {:?}", buffer);
        let bmr = buffer.map_readable().unwrap();
        let bmrsl = bmr.as_slice();
        let buffer_size: u32 = buffer.size().try_into().unwrap();
        trace!(
            CAT,
            obj: pad,
            "gstnada Handling rtcp buffer size {} ",
            buffer_size
        );
        unsafe {
            self.with_controller(|c| sender_on_feedback(c, bmrsl.as_ptr(), buffer_size as i32));
        }
      
        drop(bmr);
        // if res == 0 {
        self.rtcp_srcpad.push(buffer).unwrap();
        //   }
        glib::bitflags::_core::result::Result::Ok(gst::FlowSuccess::Ok)
    }

    // Called whenever an event arrives on the rtcp_sink pad. It has to be handled accordingly and in
    // most cases has to be either passed to Pad::event_default() on this pad for default handling,
    // or Pad::push_event() on all pads with the opposite direction for direct forwarding.
    // Here we just pass through all events directly to the source pad.
    //
    // See the documentation of gst::Event and gst::EventRef to see what can be done with
    // events, and especially the gst::EventView type for inspecting events.
    fn rtcp_sink_event(
        &self,
        pad: &gst::Pad,
        _element: &super::Gcctx,
        event: gst::Event,
    ) -> bool {
        log!(
            CAT,
            obj: pad,
            "Nada Handling rtcp_sink event {:?} {:?}",
            event,
            event.type_()
        );
        self.rtcp_srcpad.push_event(event)
    }

    // Called whenever a query is sent to the rtcp_sink pad. It has to be answered if the element can
    // handle it, potentially by forwarding the query first to the peer pads of the pads with the
    // opposite direction, or false has to be returned. Default handling can be achieved with
    // Pad::query_default() on this pad and forwarding with Pad::peer_query() on the pads with the
    // opposite direction.
    // Here we just forward all queries directly to the source pad's peers.
    //
    // See the documentation of gst::Query and gst::QueryRef to see what can be done with
    // queries, and especially the gst::QueryView type for inspecting and modifying queries.
    fn rtcp_sink_query(
        &self,
        pad: &gst::Pad,
        _element: &super::Gcctx,
        query: &mut gst::QueryRef,
    ) -> bool {
        log!(
            CAT,
            obj: pad,
            "gstnada Handling rtcp_sink query {:?}",
            query
        );
        self.rtcp_srcpad.peer_query(query)
    }

    // Called whenever an event arrives on the source pad. It has to be handled accordingly and in
    // most cases has to be either passed to Pad::event_default() on the same pad for default
    // handling, or Pad::push_event() on all pads with the opposite direction for direct
    // forwarding.
    // Here we just pass through all events directly to the sink pad.
    //
    // See the documentation of gst::Event and gst::EventRef to see what can be done with
    // events, and especially the gst::EventView type for inspecting events.
    fn src_event(&self, pad: &gst::Pad, _element: &super::Gcctx, event: gst::Event) -> bool {
        log!(
            CAT,
            obj: pad,
            "gstnada src Handling event {:?} {:?}",
            event,
            event.type_()
        );
        self.sinkpad.push_event(event)
    }

    fn rtcp_src_event(
        &self,
        pad: &gst::Pad,
        _element: &super::Gcctx,
        event: gst::Event,
    ) -> bool {
        log!(
            CAT,
            obj: pad,
            "gstnada rtcp src Handling event {:?} {:?}",
            event,
            event.type_()
        );
        true
        // self.rtcp_sinkpad.push_event(event)
    }

    // Called whenever a query is sent to the source pad. It has to be answered if the element can
    // handle it, potentially by forwarding the query first to the peer pads of the pads with the
    // opposite direction, or false has to be returned. Default handling can be achieved with
    // Pad::query_default() on this pad and forwarding with Pad::peer_query() on the pads with the
    // opposite direction.
    // Here we just forward all queries directly to the sink pad's peers.
    //
    // See the documentation of gst::Query and gst::QueryRef to see what can be done with
    // queries, and especially the gst::QueryView type for inspecting and modifying queries.
    fn src_query(
        &self,
        pad: &gst::Pad,
        _element: &super::Gcctx,
        query: &mut gst::QueryRef,
    ) -> bool {
        log!(CAT, obj: pad, "gstnada Handling src query {:?}", query);
        self.sinkpad.peer_query(query)
    }
    fn rtcp_src_query(
        &self,
        pad: &gst::Pad,
        _element: &super::Gcctx,
        query: &mut gst::QueryRef,
    ) -> bool {
        log!(
            CAT,
            obj: pad,
            "gstnada Handling rtcp src query {:?}",
            query
        );
        self.rtcp_sinkpad.peer_query(query)
    }
}

pub(crate) fn parse_twcc(data: &[u8]) -> (Vec<Feedback>, u64) {
    let mut reader = BitReader::new(data);
    let _v = reader.read_u8(2).unwrap();
    assert_eq!(_v, 2);
    let padding = reader.read_u8(1).unwrap();
    let _fmt = reader.read_u8(5).unwrap();
    //assert_eq!(_fmt, 15);
    let _pt = reader.read_u8(8).unwrap();
   // assert_eq!(_pt, 205);
    let length = reader.read_u16(16).unwrap();
    let ssrc_send = reader.read_u32(32).unwrap();
    let ssrc_media = reader.read_u32(32).unwrap();
    let base_seq = reader.read_u16(16).unwrap();
    let packet_status = reader.read_u16(16).unwrap();
    let ref_time = reader.read_u32(24).unwrap() << 6;
    let fb_pkt_count = reader.read_u32(8).unwrap();

    let mut ecn_list: Vec<Feedback> = Vec::with_capacity(fb_pkt_count as usize);

    let mut recv_blocks = 0;
    for _ in 0..fb_pkt_count {
        let pkt_type = reader.read_u8(1).unwrap();
        match pkt_type {
            0 => {
                let ecn = reader.read_u8(2).unwrap();
                let cnt = reader.read_u16(13).unwrap();
                for _ in 0..cnt {
                    ecn_list.push(Feedback::new(ecn, base_seq));
                }
            }
            1 => {
                let sym_size = reader.read_u8(1).unwrap();
                match sym_size {
                    0 => {
                        for _ in 0..14 {
                            ecn_list.push(Feedback::new(reader.read_u8(1).unwrap(), base_seq));
                        }
                    }
                    1 => {
                        for _ in 1..7 {
                            ecn_list.push(Feedback::new(reader.read_u8(2).unwrap(), base_seq));
                        }
                    }
                    _ => panic!("unexpect sym_size {}", sym_size)
                }
            }
            _ => panic!("unexpected pkt_type {}", pkt_type)
        }
    }
    let mut cur_time = ref_time;
    for x in ecn_list.iter_mut().filter(|t| t.ecn == 1 || t.ecn == 2) {
        match x.ecn {
            1 => {
                let mut offset = reader.read_u8(8).unwrap();
                // in millis, 250 µs -> 0.25ms -> 1 /4 -> >> 2
                offset = offset >> 2;
                cur_time = cur_time + offset as u32;
                x.ts = cur_time as u64;
            }
            2 => {
                let mut offset = reader.read_u16(16).unwrap();
                // in millis, 250 µs -> 0.25s -> 1 /4 -> >> 2
                offset = offset >> 2;
                cur_time = cur_time + offset as u32;
                x.ts = cur_time as u64;
            }
            _ => unreachable!()
        }
    }
    (ecn_list, reader.position() >> 3)
}

fn on_send_packet(stx: *const Gcctx, id: u32, is_push: u8) {
    trace!(
        CAT,
        "gstnada Handling buffer from scream {:?} is_push  {}",
        id,
        is_push
    );

    if is_push == 1 {
        unsafe {
            let buf =
            {
                let mut guard = (*stx).data.lock().unwrap();
                guard.remove(&id).unwrap()
            };
            let fls = (*stx).srcpad.pad_flags();
            //            if fls.contains(gst::PadFlags::FLUSHING) || fls.contains(gst::PadFlags::EOS)
            if fls.contains(gst::PadFlags::EOS) {
                println!("Gcctx EOS {:?}", fls);
                drop(buf);
            } else if fls.contains(gst::PadFlags::FLUSHING) {
                println!("Gcctx FL {:?}", fls);
                drop(buf);
            } else {
                (*stx)
                    .srcpad
                    .push(buf)
                    .expect("Gcctx callback srcpad.push failed");
            }
        }
    } else {
        unsafe { (*stx).data.lock().unwrap().remove(&id).unwrap(); }
    }
}

// This trait registers our type with the GObject object system and
// provides the entry points for creating a new instance and setting
// up the class data
#[glib::object_subclass]
impl ObjectSubclass for Gcctx {
    const NAME: &'static str = "RsGcctx";
    type Type = super::Gcctx;
    type ParentType = gst::Element;

    // Called when a new instance is to be created. We need to return an instance
    // of our struct here and also get the class struct passed in case it's needed
    fn with_class(klass: &Self::Class) -> Self {
        // Create our two pads from the templates that were registered with
        // the class and set all the functions on them.
        //
        // Each function is wrapped in catch_panic_pad_function(), which will
        // - Catch panics from the pad functions and instead of aborting the process
        //   it will simply convert them into an error message and poison the element
        //   instance
        // - Extract our Gcctx struct from the object instance and pass it to us
        //
        // Details about what each function is good for is next to each function definition
        let templ = klass.pad_template("sink").unwrap();
        let sinkpad = gst::Pad::builder_with_template(&templ, Some("sink"))
            .chain_function(|pad, parent, buffer| {
                Gcctx::catch_panic_pad_function(
                    parent,
                    || Err(gst::FlowError::Error),
                    |Gcctx, element| Gcctx.sink_chain(pad, element, buffer),
                )
            })
            .event_function(|pad, parent, event| {
                Gcctx::catch_panic_pad_function(
                    parent,
                    || false,
                    |Gcctx, element| Gcctx.sink_event(pad, element, event),
                )
            })
            .query_function(|pad, parent, query| {
                Gcctx::catch_panic_pad_function(
                    parent,
                    || false,
                    |Gcctx, element| Gcctx.sink_query(pad, element, query),
                )
            })
            .build();

        let templ = klass.pad_template("rtcp_sink").unwrap();
        let rtcp_sinkpad = gst::Pad::builder_with_template(&templ, Some("rtcp_sink"))
            .chain_function(|pad, parent, buffer| {
                Gcctx::catch_panic_pad_function(
                    parent,
                    || Err(gst::FlowError::Error),
                    |Gcctx, element| Gcctx.rtcp_sink_chain(pad, element, buffer),
                )
            })
            .event_function(|pad, parent, event| {
                Gcctx::catch_panic_pad_function(
                    parent,
                    || false,
                    |Gcctx, element| Gcctx.rtcp_sink_event(pad, element, event),
                )
            })
            .query_function(|pad, parent, query| {
                Gcctx::catch_panic_pad_function(
                    parent,
                    || false,
                    |Gcctx, element| Gcctx.rtcp_sink_query(pad, element, query),
                )
            })
            .build();

        let templ = klass.pad_template("src").unwrap();
        let srcpad = gst::Pad::builder_with_template(&templ, Some("src"))
            .event_function(|pad, parent, event| {
                Gcctx::catch_panic_pad_function(
                    parent,
                    || false,
                    |Gcctx, element| Gcctx.src_event(pad, element, event),
                )
            })
            .query_function(|pad, parent, query| {
                Gcctx::catch_panic_pad_function(
                    parent,
                    || false,
                    |Gcctx, element| Gcctx.src_query(pad, element, query),
                )
            })
            .build();

        let templ = klass.pad_template("rtcp_src").unwrap();
        let rtcp_srcpad = gst::Pad::builder_with_template(&templ, Some("rtcp_src"))
            .event_function(|pad, parent, event| {
                Gcctx::catch_panic_pad_function(
                    parent,
                    || false,
                    |Gcctx, element| Gcctx.rtcp_src_event(pad, element, event),
                )
            })
            .query_function(|pad, parent, query| {
                Gcctx::catch_panic_pad_function(
                    parent,
                    || false,
                    |Gcctx, element| Gcctx.rtcp_src_query(pad, element, query),
                )
            })
            .build();

        let settings = Mutex::new(Default::default());

        // Return an instance of our struct and also include our debug category here.
        // The debug category will be used later whenever we need to put something
        // into the debug logs
        let queue = Mutex::new(HashMap::new());
        let data = Box::leak(Box::new(Data {
            ctrl: Mutex::new(RazorController(std::ptr::null())),
            gx: std::ptr::null(),
        }));
        let mut ret = Self {
            srcpad,
            sinkpad,
            rtcp_srcpad,
            rtcp_sinkpad,
            settings,
            data: queue,
            controller: Mutex::new(RazorController(std::ptr::null_mut())),
            clock_wait: Mutex::new(ClockWait {
                clock_id: None,
                _flushing: true,
            }),
        };
        ret
    }
}

impl Gcctx {}

/**
    Returns true if the bitrate was changed.
**/
fn thread_timer(ptr: &Gcctx) -> bool {
    unsafe {
        let cur_bitrate;
        {
            cur_bitrate = ptr.settings.lock().unwrap().current_max_bitrate;
        }
        ptr.with_controller(|c| sender_cc_heartbeat(c));
        let new_bitrate;
        {
            new_bitrate = ptr.settings.lock().unwrap().current_max_bitrate;
        }
        cur_bitrate != new_bitrate
    }
}

// Implementation of glib::Object virtual methods
impl ObjectImpl for Gcctx {
    // Called right after construction of a new instance
    fn constructed(&self, obj: &Self::Type) {
        // Call the parent class' ::constructed() implementation first
        self.parent_constructed(obj);

        // Here we actually add the pads we created in Gcctx::new() to the
        // element so that GStreamer is aware of their existence.
        obj.add_pad(&self.sinkpad).unwrap();
        obj.add_pad(&self.rtcp_sinkpad).unwrap();
        obj.add_pad(&self.srcpad).unwrap();
        obj.add_pad(&self.rtcp_srcpad).unwrap();
    }
    // Called whenever a value of a property is changed. It can be called
    // at any time from any thread.

    // Metadata for the properties
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: Lazy<Vec<glib::ParamSpec>> = Lazy::new(|| {
            vec![
                glib::ParamSpecString::new(
                    "params",
                    "Params",
                    "scream lib command line args",
                    None,
                    glib::ParamFlags::READWRITE,
                ),
                glib::ParamSpecString::new(
                    "stats",
                    "Stats",
                    "Gcctx get_property lib stats in csv format",
                    None,
                    glib::ParamFlags::READWRITE,
                ),
                glib::ParamSpecString::new(
                    "stats-clear",
                    "StatsClear",
                    "Gcctx get_property lib stats in csv format and clear counters",
                    None,
                    glib::ParamFlags::READWRITE,
                ),
                glib::ParamSpecString::new(
                    "stats-header",
                    "StatsHeader",
                    "Gcctx get_property lib stats-header in csv format",
                    None,
                    glib::ParamFlags::READWRITE,
                ),
                glib::ParamSpecUInt::new(
                    "current-max-bitrate",
                    "Current-max-bitrate",
                    "Current max bitrate in kbit/sec, set by scream or by application",
                    0,
                    u32::MAX,
                    DEFAULT_CURRENT_MAX_BITRATE,
                    glib::ParamFlags::READWRITE,
                ),
            ]
        });
        PROPERTIES.as_ref()
    }

    fn signals() -> &'static [Signal] {
        use once_cell::sync::Lazy;
        static SIGNALS: Lazy<Vec<Signal>> = Lazy::new(|| {
            vec![
                Signal::builder(
                    "heartbeat",
                    &[String::static_type().into()],
                    glib::Type::UNIT.into(),
                ).build(),
            ]
        });
        SIGNALS.as_ref()
    }

    fn set_property(
        &self,
        obj: &Self::Type,
        _id: usize,
        value: &glib::Value,
        pspec: &glib::ParamSpec,
    ) {
        match pspec.name() {
            "params" => {
                let mut settings = self.settings.lock().unwrap();
                // self.state.lock().unwrap().is_none()
                settings.params = match value.get::<String>() {
                    Ok(params) => Some(params),
                    _ => unreachable!("type checked upstream"),
                };
                info!(
                    CAT,
                    obj: obj,
                    "Changing params  to {}",
                    settings.params.as_ref().unwrap()
                );
                //                self.srcpad.to_glib_none()
                // STREAMTX_PTR = Some(&self);
                //unsafe {
                //    ScreamSenderPluginInit(s.as_ptr(), self, callback);
                // }
            }
            "current-max-bitrate" => {
                let mut settings = self.settings.lock().unwrap();
                let rate = value.get().expect("type checked upstream");
                info!(
                    CAT,
                    obj: obj,
                    "Changing current-max-bitrate from {} to {}",
                    settings.current_max_bitrate,
                    rate
                );
                settings.current_max_bitrate = rate;
            }
            _ => unimplemented!(),
        }
    }

    // Called whenever a value of a property is read. It can be called
    // at any time from any thread.
    fn property(&self, _obj: &Self::Type, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        match pspec.name() {
            "params" => {
                let settings = self.settings.lock().unwrap();
                settings.params.to_value()
            }
            "stats" => {
                let res = unsafe {
                    let mut dst = Vec::with_capacity(500);
                    let mut dstlen: u32 = 0;
                    let pdst = dst.as_mut_ptr();

                    //ScreamSenderStats(pdst, &mut dstlen, 0);
                    dst.set_len(dstlen.try_into().unwrap());
                    dst
                };
                let str1 = String::from_utf8(res).unwrap();
                str1.to_value()
            }
            "stats-clear" => {
                let res = unsafe {
                    let mut dst = Vec::with_capacity(500);
                    let mut dstlen: u32 = 0;
                    let pdst = dst.as_mut_ptr();

                    //ScreamSenderStats(pdst, &mut dstlen, 1);
                    dst.set_len(dstlen.try_into().unwrap());
                    dst
                };
                let str1 = String::from_utf8(res).unwrap();
                str1.to_value()
            }
            "stats-header" => {
                let res = unsafe {
                    let mut dst = Vec::with_capacity(500);
                    let mut dstlen: u32 = 0;
                    let pdst = dst.as_mut_ptr();

                    //ScreamSenderStatsHeader(pdst, &mut dstlen);
                    dst.set_len(dstlen.try_into().unwrap());
                    dst
                };
                let str1 = String::from_utf8(res).unwrap();
                str1.to_value()
            }
            "current-max-bitrate" => {
                let settings = self.settings.lock().unwrap();
                settings.current_max_bitrate.to_value()
            }
            _ => unimplemented!(),
        }
    }
}

// Implementation of gst::Element virtual methods
impl GstObjectImpl for Gcctx {}

impl ElementImpl for Gcctx {
    // Set the element specific metadata. This information is what
    // is visible from gst-inspect-1.0 and can also be programatically
    // retrieved from the gst::Registry after initial registration
    // without having to load the plugin in memory.
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static ELEMENT_METADATA: Lazy<gst::subclass::ElementMetadata> = Lazy::new(|| {
            gst::subclass::ElementMetadata::new(
                "Gcctx",
                "Generic",
                "pass RTP packets to Gcctx",
                "Jacob Teplitsky <jacob.teplitsky@ericsson.com>",
            )
        });
        Some(&*ELEMENT_METADATA)
    }

    // Create and add pad templates for our sink and source pad. These
    // are later used for actually creating the pads and beforehand
    // already provide information to GStreamer about all possible
    // pads that could exist for this type.
    //
    // Actual instances can create pads based on those pad templates
    fn pad_templates() -> &'static [gst::PadTemplate] {
        static PAD_TEMPLATES: Lazy<Vec<gst::PadTemplate>> = Lazy::new(|| {
            // Our element can accept any possible caps on both pads
            let caps = gst::Caps::new_simple("application/x-rtp", &[]);
            let src_pad_template = gst::PadTemplate::new(
                "src",
                gst::PadDirection::Src,
                gst::PadPresence::Always,
                &caps,
            )
                .unwrap();

            let caps = gst::Caps::new_simple("application/x-rtcp", &[]);
            let rtcp_src_pad_template = gst::PadTemplate::new(
                "rtcp_src",
                gst::PadDirection::Src,
                gst::PadPresence::Always,
                &caps,
            )
                .unwrap();

            let caps = gst::Caps::new_simple("application/x-rtp", &[]);
            let sink_pad_template = gst::PadTemplate::new(
                "sink",
                gst::PadDirection::Sink,
                gst::PadPresence::Always,
                &caps,
            )
                .unwrap();

            let caps = gst::Caps::new_simple("application/x-rtcp", &[]);
            let rtp_sink_pad_template = gst::PadTemplate::new(
                "rtcp_sink",
                gst::PadDirection::Sink,
                gst::PadPresence::Always,
                &caps,
            )
                .unwrap();
            vec![
                src_pad_template,
                rtcp_src_pad_template,
                sink_pad_template,
                rtp_sink_pad_template,
            ]
        });

        PAD_TEMPLATES.as_ref()
    }

    // Called whenever the state of the element should be changed. This allows for
    // starting up the element, allocating/deallocating resources or shutting down
    // the element again.
    fn change_state(
        &self,
        element: &Self::Type,
        transition: gst::StateChange,
    ) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
        info!(CAT, obj: element, "Changing state {:?}", transition);
        match transition {
            gst::StateChange::NullToReady => {
                unsafe {
                    let ptr = sender_cc_create(cast_to_raw_pointer(self), bitrate_change, cast_to_raw_pointer(self), pace_send, 50);
                    *self.controller.lock().unwrap() = ptr;
                    sender_cc_set_bitrates(ptr, 50*1000, 1000*200, 1000*2000);
                    ptr
                };
                debug!(CAT, obj: element, "Waiting for 1s before retrying");
                let clock = gst::SystemClock::obtain();
                let wait_time = clock.time().unwrap() + gst::ClockTime::SECOND;
                let mut clock_wait = self.clock_wait.lock().unwrap();
                let timeout = clock.new_periodic_id(wait_time, gst::ClockTime::from_useconds(500));
                clock_wait.clock_id = Some(timeout.clone());
                let element_weak = element.downgrade();
                timeout
                    .wait_async(move |_clock, _time, _id| {
                        let element = match element_weak.upgrade() {
                            None => return,
                            Some(element) => element,
                        };
                        //println!("{:?}", &element.parent().unwrap());
                        if let Some(e) = element().unwrap().dynamic_cast_ref::<Bin>() {
                            if let Some(val) = e.by_name_recurse_up("rtpjitterbuffer") {
                                println!("GOT VAL {:?}", &val);
                            }
                        }
                        let lib_data = Gcctx::from_instance(&element);
                        if thread_timer(&lib_data) {
                            println!("bitrate change");
                            element.notify("current-max-bitrate")
                        }
                    })
                    .expect("Failed to wait async");
            }
            gst::StateChange::ReadyToNull => {
                let mut clock_wait = self.clock_wait.lock().unwrap();
                if let Some(clock_id) = clock_wait.clock_id.take() {
                    clock_id.unschedule();
                }
            }
            _ => (),
        }
        // Call the parent class' implementation of ::change_state()
        self.parent_change_state(element, transition)
    }
}


extern "C" fn bitrate_change(trigger: *const u8, bitrate: u32, loss: u8, rtt: u32) {
    let gcc : *const Gcctx = unsafe {
        cast_from_raw_pointer(trigger)
    };
    unsafe {
        let mut guard = (*gcc).settings.lock().unwrap();
        let old_br = guard.current_max_bitrate;
        guard.current_max_bitrate = bitrate;
        drop(guard);
        println!("Bitrate old: {}, Bitrate new: {}", old_br, bitrate);
    }
}

extern "C" fn pace_send(handler: *const u8, id: u32, retrans: i32, size: usize, pad: i32) {
    //Actually send data.
    unsafe {
        on_send_packet(cast_from_raw_pointer(handler), id, true as u8);
    }
}

#[link(name = "cc", kind = "static")]
#[link(name = "estimator", kind = "static")]
#[link(name = "common", kind = "static")]
#[link(name = "pacing", kind = "static")]
extern "C" {
    #[allow(improper_ctypes)]
    fn sender_cc_create(trigger: *const u8, bitrate_cb:
    extern "C" fn(trigger: *const u8, bitrate: u32, loss: u8, rtt: u32), handler: *const u8,
                        pace_send_func: extern "C" fn(handler: *const u8, id: u32, retrans: i32, size: usize, pad: i32),
                        queue_ms: i32) -> RazorController;
    fn sender_cc_destroy(cc: RazorController);
    fn sender_cc_heartbeat(cc: RazorController);
    fn sender_cc_add_pace_packet(cc: RazorController, packet_id: u32, retrans: i32, size: usize);
    fn sender_on_send_packet(cc: RazorController, seq: u16, size: usize);
    fn sender_on_feedback(cc: RazorController, feedback: *const u8, size: i32);
    fn sender_cc_update_rtt(cc: RazorController, rtt: i32);
    fn sender_cc_set_bitrates(cc: RazorController, min: u32, start: u32, max: u32);
    fn sender_cc_get_pacer_queue_ms(cc: RazorController);
    fn sender_cc_get_first_packet_ts(cc: RazorController);
}
