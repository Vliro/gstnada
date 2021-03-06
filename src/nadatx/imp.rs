use glib::prelude::*;
use glib::subclass::{Signal, SignalType};
use glib::subclass::prelude::*;
use gst::prelude::*;
use gst::subclass::prelude::*;
use std::convert::TryInto;
use std::ffi::CString;
use std::{thread, time};
use std::collections::VecDeque;
use std::error::Error;
use std::mem::MaybeUninit;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU16, AtomicU32, AtomicUsize, Ordering};
use std::time::Duration;
use bitreader::BitReader;
use gstreamer::{debug, error, info, log, trace};
use gstreamer_rtp::ffi::{gst_rtcp_buffer_get_first_packet, gst_rtcp_buffer_map, gst_rtcp_buffer_new, gst_rtcp_packet_move_to_next, GstRTCPBuffer, GstRTCPPacket};

use gstreamer_video as gstv;


pub use gstreamer_rtp::rtp_buffer::compare_seqnum;
pub use gstreamer_rtp::rtp_buffer::RTPBuffer;
pub use gstreamer_rtp::rtp_buffer::RTPBufferExt;
use gstreamer_sys::{GST_MAP_READ, GstBuffer, GstMapInfo};
use gstreamer_video::VideoEndianness::BigEndian;

use once_cell::sync::Lazy;
use crate::gst;

const DEFAULT_CURRENT_MAX_BITRATE: u32 = 0;

#[repr(C)]
#[derive(Copy, Clone)]
pub (crate) struct NadaController(*const std::ffi::c_void);
unsafe impl Sync for NadaController {}
unsafe impl Send for NadaController {}
// Property value storage
#[derive(Debug, Clone)]
struct Settings {
    params: Option<String>,
    current_max_bitrate: u32,
}

struct SendPtr (*const nadatx);

unsafe impl Send for SendPtr {}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            params: None,
            current_max_bitrate: DEFAULT_CURRENT_MAX_BITRATE,
        }
    }
}

// static STREAMTX_PTR: Option<&nadatx>  = None;

// Struct containing all the element data
#[repr(C)]
pub struct nadatx {
    srcpad: gst::Pad,
    sinkpad: gst::Pad,
    rtcp_srcpad: gst::Pad,
    rtcp_sinkpad: gst::Pad,
    settings: Mutex<Settings>,
    last_base: AtomicUsize,
    start_offset_for_rtcp: AtomicU16,
    data: Mutex<VecDeque<gst::Buffer>>,
    controller: NadaController
}

static CAT: Lazy<gst::DebugCategory> = Lazy::new(|| {
    gst::DebugCategory::new(
        "nadatx",
        gst::DebugColorFlags::empty(),
        Some("nadatx Element"),
    )
});



impl Drop for nadatx {
    fn drop(&mut self) {
        unsafe {
           FreeController(self.controller);
        }
    }
}
#[derive(Debug)]
pub (crate) struct Feedback {
    ecn: u8,
    seq: u16,
    ts: u64
}
//        uint16_t sequence;
//         uint64_t rxTimestampUs;
//         uint8_t ecn;
impl Feedback {
    pub fn new(ecn: u8, seq: u16) -> Feedback {
        Feedback {
            ecn,
            seq,
            ts: 0
        }
    }
}

impl nadatx {
    // Called whenever a new buffer is passed to our sink pad. Here buffers should be processed and
    // whenever some output buffer is available have to push it out of the source pad.
    // Here we just pass through all buffers directly
    //
    // See the documentation of gst::Buffer and gst::BufferRef to see what can be done with
    // buffers.
    fn sink_chain(
        &self,
        pad: &gst::Pad,
        element: &super::nadatx,
        buffer: gst::Buffer,
    ) -> Result<gst::FlowSuccess, gst::FlowError> {
        // trace!(CAT, obj: pad, "gstnada Handling buffer {:?}", buffer);
        let rtp_buffer = RTPBuffer::from_buffer_readable(&buffer).unwrap();
        let seq = rtp_buffer.seq();
        let payload_type = rtp_buffer.payload_type();
        let timestamp = time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .expect("Time went backwards").as_micros() as u64;
        let _ssrc = rtp_buffer.ssrc();
        let (_,byte) = rtp_buffer.extension_bytes().unwrap();

        let mut offset = self.start_offset_for_rtcp.load(Ordering::Relaxed);
        if offset == 0 {
            offset = u16::from_be_bytes([byte[1], byte[2]]);
            self.start_offset_for_rtcp.store(offset, Ordering::Relaxed);
            offset = 0;
        } else {
            offset = u16::from_be_bytes([byte[1], byte[2]]) - offset;
        }
        drop(byte);
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
        let mut rate: u32 = 0;
        //let mut force_idr: u32 = 1;
        let ok;
        unsafe {
            let size = buffer.size().try_into().unwrap();
            ok = OnPacket(self.controller, timestamp as u64, offset, size);
            rate = getBitrate(self.controller) as u32;
        }
        debug!(
            CAT,
            obj: pad,
            "NadaTX current estimate bitrate {}",
            rate,
        );        if !ok {
            error!(CAT, obj: element, "Errored in OnPacket in NadaTx::sink_chain");
        }
        if rate != 0 {
            let mut settings = self.settings.lock().unwrap();
            rate /= 1000;
            let are_equal = settings.current_max_bitrate == rate;
            if !are_equal {
                settings.current_max_bitrate = rate;
            }
            drop(settings);
            if !are_equal {
                element.notify("current-max-bitrate");
            }
        }
        self.data.lock().unwrap().push_back(buffer);

        let event = gstv::UpstreamForceKeyUnitEvent::builder()
            .all_headers(true)
            .build();
        self.sinkpad.push_event(event);

        glib::bitflags::_core::result::Result::Ok(gst::FlowSuccess::Ok)
    }

    // Called whenever an event arrives on the sink pad. It has to be handled accordingly and in
    // most cases has to be either passed to Pad::event_default() on this pad for default handling,
    // or Pad::push_event() on all pads with the opposite direction for direct forwarding.
    // Here we just pass through all events directly to the source pad.
    //
    // See the documentation of gst::Event and gst::EventRef to see what can be done with
    // events, and especially the gst::EventView type for inspecting events.
    fn sink_event(&self, pad: &gst::Pad, _element: &super::nadatx, event: gst::Event) -> bool {
        log!(
            CAT,
            obj: pad,
            "gstnada Handling event {:?} {:?}",
            event,
            event.type_()
        );
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
        _element: &super::nadatx,
        query: &mut gst::QueryRef,
    ) -> bool {
        log!(CAT, obj: pad, "gstnada Handling query {:?}", query);
        self.srcpad.peer_query(query)
    }

    fn rtcp_sink_chain(
        &self,
        pad: &gst::Pad,
        element: &super::nadatx,
        mut buffer: gst::Buffer,
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
        let last = self.last_base.load(Ordering::Relaxed);
        match parse_twcc(bmrsl, Some(last)) {
            Ok((data, count)) if data.len() > 0 => {
                self.last_base.store(data[0].seq as usize, Ordering::Relaxed);
                let mut ok = true;
                let since_the_epoch = time::SystemTime::now()
                    .duration_since(time::UNIX_EPOCH)
                    .expect("Time went backwards").as_micros() as u64;
                for x in data {
                    unsafe {
                        ok &= OnFeedback(self.controller,  since_the_epoch, x.seq,x.ts, x.ecn);
                    }
                }

                info!(CAT, obj: element, "nadatx parsed {} bytes", count);
                drop(bmr);
                // if res == 0 {
                if let Ok(e) = self.rtcp_srcpad.push(buffer) {
                }
                //   }
            },
            Err(e) => {
                println!("{e}");
            }
            _ => {}
        }
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
        _element: &super::nadatx,
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
        _element: &super::nadatx,
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
    fn src_event(&self, pad: &gst::Pad, _element: &super::nadatx, event: gst::Event) -> bool {
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
        _element: &super::nadatx,
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
        _element: &super::nadatx,
        query: &mut gst::QueryRef,
    ) -> bool {
        log!(CAT, obj: pad, "gstnada Handling src query {:?}", query);
        self.sinkpad.peer_query(query)
    }
    fn rtcp_src_query(
        &self,
        pad: &gst::Pad,
        _element: &super::nadatx,
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

pub (crate) fn parse_twcc(data: &[u8], last: Option<usize>) -> Result<(Vec<Feedback>, u64), Box<dyn Error>> {
    let mut reader = BitReader::new(data);
    let mut ecn_list: Vec<Feedback> = Vec::new();
    let base_seq = reader.read_u16(16)?;
    if let Some(la) = last {
        if la == base_seq as usize {
            return Ok((vec![], 0));
        }
    }

    let packet_status = reader.read_u16(16)?;
    //Ref time in ??s
    let mut ref_time = (reader.read_u32(24)? as u64)*1000000*64;
    let fb_pkt_count = reader.read_u32(8)?;
    let mut recv_blocks = 0;
    let mut packets_parsed = 0;
    ecn_list = Vec::with_capacity(packet_status as usize);
    while(packets_parsed < packet_status) {
        let remaining_pkt = packet_status - packets_parsed;
        let seqnum_offset = base_seq + packets_parsed;
        let pkt_chunk = reader.read_u16(16)?;
        if (pkt_chunk >> 15) == 1 {
            let sym_size = ((pkt_chunk & 0x4000) >> 14) + 1;
            let n_bits = std::cmp::min(remaining_pkt, 14 / sym_size);
            for i in 0..n_bits {
                ecn_list.push(Feedback::new(reader.read_u8(sym_size as u8)?, seqnum_offset + i));
            }
            packets_parsed += n_bits;
        } else {
            let ecn = (pkt_chunk & 0x6000) >> 13;
            let cnt = pkt_chunk & 0x1FFF;
            let rl = std::cmp::min(remaining_pkt, cnt);
            for i in 0..rl {
                ecn_list.push(Feedback::new(ecn as u8, seqnum_offset + i))
            }
            packets_parsed += rl;
        }
    }
    let mut ts_rounded = ref_time as u64;
    for pkt in ecn_list.iter_mut() {
        let mut delta = 0;
        if pkt.ecn == 1 {
            delta = reader.read_u8(8)?;
        } else if pkt.ecn == 2 {
            delta = reader.read_u16(16)? as u8;
        }
        if pkt.ecn != 0 {
            let mut delta_ts = (delta as u64) * (1000*250);
            ts_rounded += delta_ts as u64;
            pkt.ts = ts_rounded / 1000;
          //  println!("time: {}", pkt.ts);
        }
    }
    Ok((ecn_list, reader.position() >> 3))
}
fn callback(stx: *const nadatx, buf: gst::Buffer, is_push: u8) {
    trace!(
        CAT,
        "gstnada Handling buffer from scream {:?} is_push  {}",
        buf,
        is_push
    );
    if is_push == 1 {
        unsafe {
            let fls = (*stx).srcpad.pad_flags();
            //            if fls.contains(gst::PadFlags::FLUSHING) || fls.contains(gst::PadFlags::EOS)
            if fls.contains(gst::PadFlags::EOS) {
                println!("nadatx EOS {:?}", fls);
                drop(buf);
            } else if fls.contains(gst::PadFlags::FLUSHING) {
                println!("nadatx FL {:?}", fls);
                drop(buf);
            } else {
                //println!("pushing to srcpad");
                (*stx)
                    .srcpad
                    .push(buf)
                    .expect("nadatx callback srcpad.push failed");
            }
        }
    } else {
        drop(buf);
    }
}

#[link(name = "nada")]
extern {
    //In kbps.
    fn NewController(min: i32, max: i32) -> NadaController;
    fn FreeController(c: NadaController);
    fn OnFeedback(c: NadaController, now_us: u64, seq: u16, ts: u64, ecn: u8) -> bool;
    fn OnPacket(c: NadaController, now_us: u64, seq: u16, size: u32) -> bool;
    fn getBitrate(c: NadaController) -> f32;
}

// This trait registers our type with the GObject object system and
// provides the entry points for creating a new instance and setting
// up the class data
#[glib::object_subclass]
impl ObjectSubclass for nadatx {
    const NAME: &'static str = "Rsnadatx";
    type Type = super::nadatx;
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
        // - Extract our nadatx struct from the object instance and pass it to us
        //
        // Details about what each function is good for is next to each function definition
        let templ = klass.pad_template("sink").unwrap();
        let sinkpad = gst::Pad::builder_with_template(&templ, Some("sink"))
            .chain_function(|pad, parent, buffer| {
                nadatx::catch_panic_pad_function(
                    parent,
                    || Err(gst::FlowError::Error),
                    |nadatx, element| nadatx.sink_chain(pad, element, buffer),
                )
            })
            .event_function(|pad, parent, event| {
                nadatx::catch_panic_pad_function(
                    parent,
                    || false,
                    |nadatx, element| nadatx.sink_event(pad, element, event),
                )
            })
            .query_function(|pad, parent, query| {
                nadatx::catch_panic_pad_function(
                    parent,
                    || false,
                    |nadatx, element| nadatx.sink_query(pad, element, query),
                )
            })
            .build();

        let templ = klass.pad_template("rtcp_sink").unwrap();
        let rtcp_sinkpad = gst::Pad::builder_with_template(&templ, Some("rtcp_sink"))
            .chain_function(|pad, parent, buffer| {
                nadatx::catch_panic_pad_function(
                    parent,
                    || Err(gst::FlowError::Error),
                    |nadatx, element| nadatx.rtcp_sink_chain(pad, element, buffer),
                )
            })
            .event_function(|pad, parent, event| {
                nadatx::catch_panic_pad_function(
                    parent,
                    || false,
                    |nadatx, element| nadatx.rtcp_sink_event(pad, element, event),
                )
            })
            .query_function(|pad, parent, query| {
                nadatx::catch_panic_pad_function(
                    parent,
                    || false,
                    |nadatx, element| nadatx.rtcp_sink_query(pad, element, query),
                )
            })
            .build();

        let templ = klass.pad_template("src").unwrap();
        let srcpad = gst::Pad::builder_with_template(&templ, Some("src"))
            .event_function(|pad, parent, event| {
                nadatx::catch_panic_pad_function(
                    parent,
                    || false,
                    |nadatx, element| nadatx.src_event(pad, element, event),
                )
            })
            .query_function(|pad, parent, query| {
                nadatx::catch_panic_pad_function(
                    parent,
                    || false,
                    |nadatx, element| nadatx.src_query(pad, element, query),
                )
            })
            .build();

        let templ = klass.pad_template("rtcp_src").unwrap();
        let rtcp_srcpad = gst::Pad::builder_with_template(&templ, Some("rtcp_src"))
            .event_function(|pad, parent, event| {
                nadatx::catch_panic_pad_function(
                    parent,
                    || false,
                    |nadatx, element| nadatx.rtcp_src_event(pad, element, event),
                )
            })
            .query_function(|pad, parent, query| {
                nadatx::catch_panic_pad_function(
                    parent,
                    || false,
                    |nadatx, element| nadatx.rtcp_src_query(pad, element, query),
                )
            })
            .build();

        let settings = Mutex::new(Default::default());

        // Return an instance of our struct and also include our debug category here.
        // The debug category will be used later whenever we need to put something
        // into the debug logs
        let controller = unsafe {
            let ptr = NewController(10, 1500);
            ptr
        };

        let at = AtomicUsize::new(0);
        let b = AtomicU16::new(0);
        let queue = Mutex::new(VecDeque::new());
        Self {
            srcpad,
            sinkpad,
            rtcp_srcpad,
            rtcp_sinkpad,
            settings,
            last_base: at,
            start_offset_for_rtcp: b,
            data: queue,
            controller
        }
    }
}

impl nadatx {

}

fn thread_timer(ptr : *const nadatx) {
    let mut guard = unsafe {(*ptr).data.lock().unwrap()};
    while let Some(obj) = guard.pop_front() {
        callback(ptr, obj, true as u8);
    }
}

// Implementation of glib::Object virtual methods
impl ObjectImpl for nadatx {
    // Called right after construction of a new instance
    fn constructed(&self, obj: &Self::Type) {
        // Call the parent class' ::constructed() implementation first
        self.parent_constructed(obj);

        // Here we actually add the pads we created in nadatx::new() to the
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
                    "nadatx get_property lib stats in csv format",
                    None,
                    glib::ParamFlags::READWRITE,
                ),
                glib::ParamSpecString::new(
                    "stats-clear",
                    "StatsClear",
                    "nadatx get_property lib stats in csv format and clear counters",
                    None,
                    glib::ParamFlags::READWRITE,
                ),
                glib::ParamSpecString::new(
                    "stats-header",
                    "StatsHeader",
                    "nadatx get_property lib stats-header in csv format",
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
                let s0 = settings.params.as_ref().unwrap().as_str();
                let s = CString::new(s0).expect("CString::new failed");
                let ptr : SendPtr = SendPtr(self);
                thread::spawn( || {
                    let p = ptr;
                    loop {
                        thread::sleep(Duration::from_millis(5));
                        thread_timer(p.0);
                    }
                });
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
impl GstObjectImpl for nadatx {

}
impl ElementImpl for nadatx {
    // Set the element specific metadata. This information is what
    // is visible from gst-inspect-1.0 and can also be programatically
    // retrieved from the gst::Registry after initial registration
    // without having to load the plugin in memory.
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static ELEMENT_METADATA: Lazy<gst::subclass::ElementMetadata> = Lazy::new(|| {
            gst::subclass::ElementMetadata::new(
                "nadatx",
                "Generic",
                "pass RTP packets to nadatx",
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

        // Call the parent class' implementation of ::change_state()
        self.parent_change_state(element, transition)
    }
}
