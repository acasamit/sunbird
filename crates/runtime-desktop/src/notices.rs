//! Notification banners drawn inside Discord (`phoenix.ui.toast` and
//! `phoenix.ui.notify`), pushed onto the overlays [`crate::modal`] hooks.
//!
//! They replace tray balloons, which are easy to miss and on some Windows
//! setups never show at all.

use std::{
   ffi::c_void,
   sync::{
      Condvar,
      Mutex,
      MutexGuard,
      PoisonError,
      atomic::{
         AtomicBool,
         AtomicU64,
         Ordering,
      },
   },
   time::{
      Duration,
      Instant,
   },
};

use crate::gpui::{
   self,
   BtnSize,
   BtnVariant,
   RawDiv,
   RcFnVtbl,
   argb_to_hsla,
   gpui_px,
};

/// Discord's blurple.
pub const ACCENT: u32 = 0xFF58_65F2;
pub const DURATION: Duration = Duration::from_secs(5);

/// The newest banners shown at once. The others wait until one is closed.
const MAX_SHOWN: usize = 3;
/// Banners kept, shown or waiting: past it the oldest go.
const MAX_KEPT: usize = 50;
const WIDTH: f32 = 340.0;
const MARGIN: f32 = 16.0;

const BACKGROUND: u32 = 0xFA2B_2D31;
const BORDER: u32 = 0xFF3F_4147;
const TITLE: u32 = 0xFFF2_F3F5;
const MUTED: u32 = 0xFFB5_BAC1;
/// A banner picture's side, and a square one's corner radius.
const PICTURE: f32 = 40.0;
const PICTURE_RADIUS: f32 = 10.0;
/// What is left of the card's width beside a picture, without the padding on
/// both sides (24), the border (2) and the gap (10).
const BESIDE_PICTURE: f32 = WIDTH - 24.0 - 2.0 - PICTURE - 10.0;

const POSITION_ABSOLUTE: u8 = 1;

/// A picture on a banner's left, like an avatar or a server icon.
#[derive(Clone)]
pub struct Picture {
   /// An `https` URL, which GPUI fetches and caches.
   pub url:   String,
   /// A circle, else a rounded square.
   pub round: bool,
}

#[derive(Clone)]
pub struct Notice {
   /// The plugin that sent it, or Sunbird.
   pub source:    String,
   pub title:     String,
   pub body:      Option<String>,
   pub accent:    u32,
   /// How long it stays, or until it is dismissed.
   pub duration:  Option<Duration>,
   /// Whether it has an Open button, which runs the plugin's `onPress`.
   pub pressable: bool,
   pub picture:   Option<Picture>,
}

impl Notice {
   /// A title and body from `source`, gone after [`DURATION`].
   pub fn plain(source: &str, title: &str, body: Option<&str>) -> Self {
      Self {
         source:    source.to_owned(),
         title:     title.to_owned(),
         body:      body.map(str::to_owned),
         accent:    ACCENT,
         duration:  Some(DURATION),
         pressable: false,
         picture:   None,
      }
   }
}

struct Shown {
   id:     u64,
   notice: Notice,
   /// Set when it first shows, so a banner waiting behind the shown ones keeps
   /// its whole duration.
   until:  Option<Instant>,
}

static READY: AtomicBool = AtomicBool::new(false);
static SHOWN: Mutex<Vec<Shown>> = Mutex::new(Vec::new());
/// Wakes [`expire`] when a banner starts counting down.
static WAKE: Condvar = Condvar::new();

fn shown() -> MutexGuard<'static, Vec<Shown>> {
   SHOWN.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Turns banners on once the overlay hook is in, and sends Sunbird's own
/// toasts and notifications to them.
pub fn enable() {
   if READY.swap(true, Ordering::AcqRel) {
      return;
   }
   std::thread::spawn(expire);
   phoenix_ui_desktop::show_in_app(|title, body| {
      show(Notice::plain("Sunbird", title, body)).is_some()
   });
}

/// Shows `notice` and returns its ID, or `None` when banners cannot be drawn
/// on this build.
pub fn show(notice: Notice) -> Option<u64> {
   static NEXT: AtomicU64 = AtomicU64::new(1);

   if !READY.load(Ordering::Acquire) {
      return None;
   }
   let id = NEXT.fetch_add(1, Ordering::Relaxed);
   {
      let mut shown = shown();
      shown.push(Shown {
         id,
         notice,
         until: None,
      });
      let extra = shown.len().saturating_sub(MAX_KEPT);
      shown.drain(..extra);
   }
   crate::modal::force_redraw();
   Some(id)
}

/// Drops banners as they run out and redraws Discord, which nothing else does
/// then.
#[expect(
   clippy::significant_drop_tightening,
   reason = "the condvar waits take the guard"
)]
fn expire() {
   loop {
      let now = Instant::now();
      let mut shown = shown();
      let before = shown.len();
      shown.retain(|shown| shown.until.is_none_or(|until| until > now));
      if shown.len() != before {
         drop(shown);
         crate::modal::force_redraw();
         continue;
      }
      let next = shown.iter().filter_map(|shown| shown.until).min();
      drop(match next {
         Some(until) => {
            WAKE
               .wait_timeout(shown, until - now)
               .map_or_else(|err| err.into_inner().0, |(shown, _)| shown)
         },
         None => WAKE.wait(shown).unwrap_or_else(PoisonError::into_inner),
      });
   }
}

pub fn dismiss(id: u64) {
   shown().retain(|shown| shown.id != id);
   crate::modal::force_redraw();
}

pub fn is_shown(id: u64) -> bool {
   shown().iter().any(|shown| shown.id == id)
}

/// Drops the banners `plugin` sent.
pub fn remove_plugin(plugin: &str) {
   let mut shown = shown();
   let before = shown.len();
   shown.retain(|shown| shown.notice.source != plugin);
   if shown.len() != before {
      drop(shown);
      crate::modal::force_redraw();
   }
}

static BUTTON_VTBL: RcFnVtbl = RcFnVtbl {
   drop_in_place: std::ptr::null(),
   size:          size_of::<u64>(),
   align:         align_of::<u64>(),
   call_once:     button_callback as *const c_void,
   call_mut:      button_callback as *const c_void,
   call:          button_callback as *const c_void,
   extra:         std::ptr::null(),
};

/// The low bit of a button's value is set on Open and clear on close; the rest
/// is the banner's ID.
fn button_value(id: u64, open: bool) -> u64 {
   (id << 1) | u64::from(open)
}

/// Mana `Button::on_click` calls `Fn::call(&self, event, window, app)`.
unsafe extern "C" fn button_callback(
   self_ptr: *const u64,
   _event: *const c_void,
   _window: *mut c_void,
   _app: *mut c_void,
) {
   if self_ptr.is_null() {
      return;
   }
   let _ = std::panic::catch_unwind(|| {
      // SAFETY: `self_ptr` points to the `u64` in the button's `RcInner`.
      let value = unsafe { *self_ptr };
      let id = value >> 1;
      dismiss(id);
      if value & 1 == 1 {
         // Off the render thread, as modal page buttons do.
         std::thread::spawn(move || crate::js::ui::press_notice(id));
      }
   });
}

fn text(text: &str, argb: u32, font_px: f32) -> Option<RawDiv> {
   let mut div = gpui::raw_div()?;
   gpui::style_div(&mut div, |s| {
      s.length(s.at.font_size, gpui_px(font_px));
      s.length(s.at.line_height, gpui_px(font_px + 5.0));
      s.hsla(s.at.text_color, argb_to_hsla(argb));
   });
   let mut any = gpui::str_to_any(text)?;
   gpui::push_any(&mut div, &mut any).then_some(div)
}

/// One banner. Button IDs go by `slot`, so they stay a handful however many
/// banners come and go.
fn banner(slot: usize, id: u64, notice: &Notice) -> Option<RawDiv> {
   let mut dot = gpui::raw_div()?;
   gpui::style_div(&mut dot, |s| {
      s.lengths(&s.at.size, gpui_px(8.0));
      s.lengths(&s.at.corner_radii, gpui_px(4.0));
      s.solid_background(argb_to_hsla(notice.accent));
   });
   let mut source = gpui::h_row(6.0, false)?;
   source = gpui::push_div(source, dot);
   source = gpui::push_div(source, text(&notice.source, MUTED, 12.0)?);
   let mut header = gpui::h_row(8.0, true)?;
   header = gpui::push_div(header, source);
   if let Some(close) = gpui::build_raw_button(
      &format!("sunbird-notice-{slot}-close"),
      "\u{d7}",
      BtnVariant::Secondary,
      BtnSize::Small,
      &raw const BUTTON_VTBL,
      button_value(id, false),
   ) {
      header = gpui::push_raw_button(header, close);
   }

   let mut card = gpui::v_stack(4.0)?;
   gpui::style_div(&mut card, |s| {
      s.lengths(&s.at.padding, gpui_px(12.0));
      s.lengths(&s.at.corner_radii, gpui_px(8.0));
      s.lengths(&s.at.border_widths, gpui_px(1.0));
      s.hsla(s.at.border_color, argb_to_hsla(BORDER));
      s.solid_background(argb_to_hsla(BACKGROUND));
   });
   card = gpui::push_div(card, header);
   let mut words = gpui::v_stack(4.0)?;
   words = gpui::push_div(words, text(&notice.title, TITLE, 14.0)?);
   if let Some(body) = notice.body.as_deref().filter(|body| !body.is_empty()) {
      words = gpui::push_div(words, text(body, MUTED, 13.0)?);
   }
   let picture = notice.picture.as_ref().and_then(|picture| {
      let radius = if picture.round {
         PICTURE / 2.0
      } else {
         PICTURE_RADIUS
      };
      gpui::image_any(&picture.url, PICTURE, radius)
   });
   if let Some(mut picture) = picture {
      gpui::style_div(&mut words, |s| {
         s.lengths(&s.at.size[..1], gpui_px(BESIDE_PICTURE));
      });
      let mut row = gpui::h_row(10.0, false)?;
      gpui::push_any(&mut row, &mut picture);
      card = gpui::push_div(card, gpui::push_div(row, words));
   } else {
      card = gpui::push_div(card, words);
   }
   if notice.pressable
      && let Some(open) = gpui::build_raw_button(
         &format!("sunbird-notice-{slot}-open"),
         "Open",
         BtnVariant::Primary,
         BtnSize::Small,
         &raw const BUTTON_VTBL,
         button_value(id, true),
      )
   {
      let row = gpui::h_row(8.0, false)?;
      card = gpui::push_div(card, gpui::push_raw_button(row, open));
   }
   Some(card)
}

/// The newest banners still up, stacked in the window's bottom right corner
/// under a count of those waiting, or `None` when there are none.
pub fn build() -> Option<RawDiv> {
   let now = Instant::now();
   let (up, waiting): (Vec<(u64, Notice)>, usize) = {
      let mut shown = shown();
      shown.retain(|shown| shown.until.is_none_or(|until| until > now));
      let waiting = shown.len().saturating_sub(MAX_SHOWN);
      let mut started = false;
      for entry in &mut shown[waiting..] {
         if entry.until.is_none()
            && let Some(duration) = entry.notice.duration
         {
            // A `durationMs` past what `Instant` holds stays until closed.
            entry.until = now.checked_add(duration);
            started |= entry.until.is_some();
         }
      }
      let up = shown[waiting..]
         .iter()
         .map(|shown| (shown.id, shown.notice.clone()))
         .collect();
      drop(shown);
      if started {
         WAKE.notify_one();
      }
      (up, waiting)
   };
   if up.is_empty() {
      return None;
   }
   let mut stack = gpui::v_stack(8.0)?;
   gpui::style_div(&mut stack, |s| {
      s.set(s.at.position, POSITION_ABSOLUTE);
      s.length(s.at.inset[1], gpui_px(MARGIN));
      s.length(s.at.inset[2], gpui_px(MARGIN));
      s.length(s.at.size[0], gpui_px(WIDTH));
   });
   if waiting > 0 {
      stack = gpui::push_div(stack, text(&format!("+{waiting} more"), MUTED, 12.0)?);
   }
   for (slot, (id, notice)) in up.iter().enumerate() {
      if let Some(banner) = banner(slot, *id, notice) {
         stack = gpui::push_div(stack, banner);
      }
   }
   Some(stack)
}
