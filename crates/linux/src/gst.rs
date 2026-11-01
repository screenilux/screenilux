//! What every GStreamer pipeline here shares: starting the library once,
//! building a pipeline from a description, and waiting on its bus.

use std::sync::Once;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;

/// Start GStreamer, once per process. Errors are reported by the first
/// pipeline that fails to build, which says more than this could.
pub fn init() {
    static START: Once = Once::new();
    START.call_once(|| {
        if let Err(e) = gst::init() {
            tracing::error!(error = %e, "GStreamer did not start");
        }
    });
}

/// A pipeline from a `gst-launch` style description.
pub fn pipeline(description: &str) -> Result<gst::Pipeline, String> {
    init();
    let element = gst::parse::launch(description).map_err(|e| e.to_string())?;
    element
        .downcast::<gst::Pipeline>()
        .map_err(|_| "the description did not make a pipeline".to_string())
}

/// A named element of `pipeline`, as the type it is.
pub fn element<T: IsA<gst::Element>>(pipeline: &gst::Pipeline, name: &str) -> Result<T, String> {
    pipeline
        .by_name(name)
        .ok_or_else(|| format!("the pipeline has no {name}"))?
        .downcast::<T>()
        .map_err(|_| format!("{name} is not the expected kind of element"))
}

/// Whether an element is installed, such as an encoder from an optional
/// plugin set.
pub fn has(factory: &str) -> bool {
    init();
    gst::ElementFactory::find(factory).is_some()
}

/// The first of `factories` that is installed.
pub fn first_of<'a>(factories: &[&'a str]) -> Option<&'a str> {
    factories.iter().copied().find(|f| has(f))
}

/// Wait for the end of the stream or an error, whichever comes first.
pub fn wait_for_end(pipeline: &gst::Pipeline, timeout: Duration) -> Result<(), String> {
    let bus = pipeline.bus().ok_or("the pipeline has no bus")?;
    let msg = bus.timed_pop_filtered(
        gst::ClockTime::from_nseconds(timeout.as_nanos() as u64),
        &[gst::MessageType::Eos, gst::MessageType::Error],
    );
    match msg.as_ref().map(|m| m.view()) {
        Some(gst::MessageView::Eos(_)) => Ok(()),
        Some(gst::MessageView::Error(e)) => Err(e.error().to_string()),
        _ => Err("timed out waiting for the stream to finish".into()),
    }
}

/// The first error on the bus so far, if any. Never blocks.
pub fn error_so_far(pipeline: &gst::Pipeline) -> Option<String> {
    let bus = pipeline.bus()?;
    let msg = bus.pop_filtered(&[gst::MessageType::Error])?;
    match msg.view() {
        gst::MessageView::Error(e) => Some(e.error().to_string()),
        _ => None,
    }
}

/// Move `pipeline` to `state` and wait for it to get there.
pub fn set_state(pipeline: &gst::Pipeline, state: gst::State) -> Result<(), String> {
    pipeline.set_state(state).map_err(|_| {
        error_so_far(pipeline).unwrap_or_else(|| format!("could not reach {state:?}"))
    })?;
    let (result, _, _) = pipeline.state(gst::ClockTime::from_seconds(10));
    result
        .map(|_| ())
        .map_err(|_| error_so_far(pipeline).unwrap_or_else(|| format!("could not reach {state:?}")))
}

pub fn seconds(t: gst::ClockTime) -> f64 {
    t.nseconds() as f64 / 1e9
}

pub fn clock_time(seconds: f64) -> gst::ClockTime {
    gst::ClockTime::from_nseconds((seconds.max(0.0) * 1e9) as u64)
}

/// Quote a path for a pipeline description.
pub fn quoted(path: &std::path::Path) -> String {
    format!(
        "\"{}\"",
        path.display()
            .to_string()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    )
}
