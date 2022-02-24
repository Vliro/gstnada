use failure::Error;
use std::{env, thread};
use std::fmt::Debug;

extern crate argparse;
extern crate failure;
extern crate gstreamer_video as gstv;
#[macro_use]
extern crate lazy_static;
use gstreamer::traits::ClockExt;
use crate::gst::glib::Cast;

use crate::gstv::prelude::ElementExt;
use crate::gstv::prelude::GstObjectExt;

use argparse::{ArgumentParser, StoreOption, StoreTrue};
use glib::{ObjectExt, StaticType};
use gst::{Clock, ClockTime, PeriodicClockId, SystemClock};
use gst::Format::Default;
use gst::prelude::{ClockExtManual, GstBinExt};
use gstreamer_sys::{gst_clock_id_wait, gst_clock_new_periodic_id, GstBin, GstClock};
use gtypes::guint;

extern crate gstreamer as gst;

mod sender_util;
//Pipeline
// videotestsrc is-live=true pattern=ball ! x264enc name=video threads=4 speed-preset=ultrafast tune=fastdecode+zerolatency bitrate=10000 ! queue max-size-buffers=2 max-size-bytes=0 max-size-time=0 ! rtph264pay ssrc=1 config-interval=-1 ! queue max-size-buffers=2 max-size-bytes=0 max-size-time=0 !  gcctx name="gcctx" params="  -nosummary -forceidr -ect 1  -initrate 2500 -minrate 500  -maxrate 10000 " !    udpsink host=127.0.0.1 port=30112 sync=false rtpbin name=r udpsrc port=30112 address=127.0.0.2 ! queue ! gcctx.rtcp_sink gcctx.rtcp_src !  r.recv_rtcp_sink_0  
fn main() {
    gst::init().expect("Failed to initialize gst_init");

    let main_loop = glib::MainLoop::new(None, false);
    start(&main_loop).expect("Failed to start");
}

pub fn start(main_loop: &glib::MainLoop) -> Result<(), Error> {
    let mut ratemultiply_opt: Option<i32> = None;
    let mut verbose = false;
    {
        // this block limits scope of borrows by ap.refer() method
        let mut ap = ArgumentParser::new();
        ap.set_description("Sender");
        ap.refer(&mut verbose)
            .add_option(&["-v", "--verbose"], StoreTrue, "Be verbose");
        ap.refer(&mut ratemultiply_opt).add_option(
            &["-r", "--ratemultiply"],
            StoreOption,
            "Set ratemultiply",
        );

        ap.parse_args_or_exit();
    }

    let pls = env::var("SENDPIPELINE").unwrap();
    println!("Pipeline: {}", pls);
    let pipeline = gst::parse_launch(&pls).unwrap();
    //let clock = pipeline_el.clock();
    let time_id = SystemClock::obtain().new_periodic_id(SystemClock::obtain().time().unwrap(), ClockTime::from_mseconds(100));

    let pipeline = pipeline.downcast::<gst::Pipeline>().unwrap();

    pipeline
        .set_state(gst::State::Playing)
        .expect("Failed to set pipeline to `Playing`");

    let pipeline = pipeline.downcast::<gst::Pipeline>().unwrap();

    let pipeline_clone = pipeline;
    /*  TBD
     * set ecn bits
     */
    sender_util::stats(&pipeline_clone, &Some("gcctx".to_string()));
    sender_util::run_time_bitrate_set(
        &pipeline_clone,
        verbose,
        &Some("gcctx".to_string()),
        &Some("video".to_string()),
        ratemultiply_opt,
    );
    let main_loop_cloned = main_loop.clone();
    let bus = pipeline_clone.bus().unwrap();
    bus.add_watch(move |_, msg| {
        use gst::MessageView;
        // println!("sender: {:?}", msg.view());
        match msg.view() {
            MessageView::Eos(..) => {
                println!("Bus watch  Got eos");
                main_loop_cloned.quit();
            }
            MessageView::Error(err) => {
                println!(
                    "Error from {:?}: {} ({:?})",
                    err.src().map(|s| s.path_string()),
                    err.error(),
                    err.debug()
                );
            }
            _ => (),
        };
        glib::Continue(true)
    })
    .expect("failed to add bus watch");
    let p = pipeline_clone.clone();
    thread::spawn(move || {
        let bin = p.dynamic_cast_ref::<gst::Bin>().unwrap();
        loop {
            time_id.wait();
            let el = bin.by_name_recurse_up("gcctx").unwrap();
            let video = bin.by_name_recurse_up("video").unwrap();
            let prop = el.property::<u32>("current-max-bitrate");
            //if prop > 0 {
            //    video.set_property("bitrate", 1000 as guint);
            //}
            //println!("Bandwidth {}",prop);
           // println!("Video bandwidth {}",video.property::<u32>("bitrate"));
        }
    });

    main_loop.run();
    pipeline_clone
        .set_state(gst::State::Null)
        .expect("Failed to set pipeline to `Null`");
    println!("Done");
    Ok(())
}
