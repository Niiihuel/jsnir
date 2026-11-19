//! Real presentation timestamps with wp_presentation (presentation-time, stable Wayland).
//! It shares winit's connection: its wl_display and wl_surface, with a separate event queue.
//! `feedback` is requested during the frame, before eframe swaps, so it belongs to that frame.
use std::ffi::c_void;
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
    backend::{Backend, ObjectId},
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_registry::WlRegistry, wl_surface::WlSurface},
};
use wayland_protocols::wp::presentation_time::client::{
    wp_presentation::{self, WpPresentation},
    wp_presentation_feedback::{self, WpPresentationFeedback},
};

#[derive(Default)]
pub struct Feedback {
    /// Compositor clock (`clock_id`); our own timestamps use the same clock.
    pub clock: Option<u32>,
    /// (frame, presented at ns, next refresh in ns, `kind` flags).
    pub presented: Vec<(u64, u64, u32, u32)>,
    pub discarded: Vec<u64>,
}
pub struct Presentation {
    conn: Connection,
    queue: EventQueue<Feedback>,
    qh: QueueHandle<Feedback>,
    presentation: WpPresentation,
    surface: WlSurface,
    pub feedback: Feedback,
}
impl Presentation {
    /// # Safety
    /// `display` and `surface` are winit's pointers and must outlive this object.
    pub unsafe fn new(display: *mut c_void, surface: *mut c_void) -> Result<Self, String> {
        let conn =
            Connection::from_backend(unsafe { Backend::from_foreign_display(display.cast()) });
        let (globals, mut queue) =
            registry_queue_init::<Feedback>(&conn).map_err(|e| e.to_string())?;
        let qh = queue.handle();
        let presentation: WpPresentation = globals
            .bind(&qh, 1..=2, ())
            .map_err(|e| format!("the compositor does not offer wp_presentation: {e}"))?;
        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.cast()) }
            .map_err(|e| e.to_string())?;
        let surface = WlSurface::from_id(&conn, id).map_err(|e| e.to_string())?;
        let mut feedback = Feedback::default();
        queue.roundtrip(&mut feedback).map_err(|e| e.to_string())?;
        Ok(Self {
            conn,
            queue,
            qh,
            presentation,
            surface,
            feedback,
        })
    }
    /// Requests presentation feedback for the content submitted next.
    pub fn request(&self, frame: u64) {
        self.presentation.feedback(&self.surface, &self.qh, frame);
        let _ = self.conn.flush();
    }
    /// Processes the feedback winit already read from the socket and routed to this queue. It does not read
    /// the socket: reading here could leave events in winit's queue that winit does not know to dispatch.
    pub fn poll(&mut self) {
        let _ = self.queue.dispatch_pending(&mut self.feedback);
    }
}
impl Dispatch<WlRegistry, GlobalListContents> for Feedback {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<WpPresentation, ()> for Feedback {
    fn event(
        state: &mut Self,
        _: &WpPresentation,
        event: wp_presentation::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            state.clock = Some(clk_id);
        }
    }
}
impl Dispatch<WpPresentationFeedback, u64> for Feedback {
    fn event(
        state: &mut Self,
        _: &WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        frame: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                refresh,
                flags,
                ..
            } => {
                let secs = ((tv_sec_hi as u64) << 32) | tv_sec_lo as u64;
                let flags = match flags {
                    WEnum::Value(k) => k.bits(),
                    WEnum::Unknown(v) => v,
                };
                state.presented.push((
                    *frame,
                    secs * 1_000_000_000 + tv_nsec as u64,
                    refresh,
                    flags,
                ));
            }
            wp_presentation_feedback::Event::Discarded => state.discarded.push(*frame),
            _ => {}
        }
    }
}
#[repr(C)]
struct Timespec {
    sec: i64,
    nsec: i64,
}
unsafe extern "C" {
    fn clock_gettime(clock: i32, ts: *mut Timespec) -> i32;
}
/// Now, on the clock the compositor announced.
pub fn now_ns(clock: u32) -> u64 {
    let mut ts = Timespec { sec: 0, nsec: 0 };
    assert_eq!(
        unsafe { clock_gettime(clock as i32, &mut ts) },
        0,
        "clock_gettime"
    );
    ts.sec as u64 * 1_000_000_000 + ts.nsec as u64
}
