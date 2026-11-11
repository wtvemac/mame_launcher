// By: Eric MacDonald (eMac)

// This helps keep the launcher on the screen. Tells the UI if it doesn't fit so it can resize, move up or add a scrollbar to fit.

use std::cell::RefCell;
use std::time::Duration;

use slint::ComponentHandle;
use slint::winit_030::{winit, WinitWindowAccessor};

use crate::MainWindow;

const SETUP_RETRY_DELAY: Duration = Duration::from_millis(100);
const SETUP_RETRY_COUNT: u32 = 50;
const SETTLE_DELAY: Duration = Duration::from_millis(80);

thread_local! {
	static MAIN: RefCell<Option<slint::Weak<MainWindow>>> = const { RefCell::new(None) };
	static LIMIT_TIMER: slint::Timer = slint::Timer::default();
	static ON_SCREEN_TIMER: slint::Timer = slint::Timer::default();
}

#[derive(Clone, Copy, Debug)]
struct Rect {
	x: i32,
	y: i32,
	width: i32,
	height: i32
}

impl Rect {
	fn bottom(&self) -> i32 {
		self.y + self.height
	}

	fn right(&self) -> i32 {
		self.x + self.width
	}

	#[cfg(all(unix, not(target_os = "macos")))]
	fn intersect(&self, other: &Rect) -> Option<Rect> {
		let x = self.x.max(other.x);
		let y = self.y.max(other.y);

		let right = self.right().min(other.right());
		let bottom = self.bottom().min(other.bottom());

		if right > x && bottom > y {
			Some(
				Rect {
					x: x,
					y: y,
					width: (right - x),
					height: (bottom - y)
				}
			)
		} else {
			None
		}
	}
}

pub fn setup(ui: &MainWindow) {
	MAIN.with(|main| *main.borrow_mut() = Some(ui.as_weak()));

	let ui_weak = ui.as_weak();
	ui.on_launcher_height_changed(move || {
		let Some(ui) = ui_weak.upgrade() else { return };

		if !ui.get_mame_console_enabled() {
			keep_on_screen_later();
		}
	});

	wait_for_window(ui.as_weak(), SETUP_RETRY_COUNT);
}

fn wait_for_window(ui_weak: slint::Weak<MainWindow>, retries_left: u32) {
	slint::Timer::single_shot(if retries_left == SETUP_RETRY_COUNT { Duration::ZERO } else { SETUP_RETRY_DELAY }, move || {
		let Some(ui) = ui_weak.upgrade() else { return };

		if ui.window().with_winit_window(|_| ()).is_some() {
			update_limit(&ui);
			keep_on_screen_later();
		} else if retries_left > 0 {
			wait_for_window(ui_weak, retries_left - 1);
		}
	});
}

pub fn on_main_window_event(event: &winit::event::WindowEvent) {
	match event {
		winit::event::WindowEvent::Moved(_) | winit::event::WindowEvent::ScaleFactorChanged { .. } => {
			LIMIT_TIMER.with(|timer| {
				timer.start(slint::TimerMode::SingleShot, Duration::from_millis(200), || {
					if let Some(ui) = main_window() {
						update_limit(&ui);
					}
				});
			});
		},
		_ => {}
	}
}

fn main_window() -> Option<MainWindow> {
	MAIN.with(|main| main.borrow().as_ref().and_then(|ui_weak| ui_weak.upgrade()))
}

pub fn update_limit(ui: &MainWindow) {
	let Some((work_area, decoration_height)) = work_area_and_decoration(ui) else { return };

	let scale_factor = ui.window().scale_factor();
	let max_height = ((work_area.height - decoration_height) as f32 / scale_factor).max(0.0);

	if (ui.get_launcher_max_height() - max_height).abs() > 0.5 {
		ui.set_launcher_max_height(max_height);
	}
}

// Largest logical inner size that fits in the work area.
pub fn fit_size(ui: &MainWindow, width: f32, height: f32) -> (f32, f32) {
	let Some((work_area, decoration_height)) = work_area_and_decoration(ui) else { return (width, height) };

	let scale_factor = ui.window().scale_factor();
	let decoration_width = ui.window().with_winit_window(|winit_window| {
		(winit_window.outer_size().width as i32 - winit_window.inner_size().width as i32).max(0)
	}).unwrap_or(0);

	let max_width = (work_area.width - decoration_width) as f32 / scale_factor;
	let max_height = (work_area.height - decoration_height) as f32 / scale_factor;

	(width.min(max_width).max(1.0), height.min(max_height).max(1.0))
}

pub fn keep_on_screen_later() {
	ON_SCREEN_TIMER.with(|timer| {
		timer.start(slint::TimerMode::SingleShot, SETTLE_DELAY, || {
			if let Some(ui) = main_window() {
				keep_on_screen(&ui);
			}
		});
	});
}

pub fn keep_on_screen(ui: &MainWindow) {
	let Some((work_area, _)) = work_area_and_decoration(ui) else { return };

	ui.window().with_winit_window(|winit_window| {
		let Ok(position) = winit_window.outer_position() else { return };
		let size = winit_window.outer_size();
		let (width, height) = (size.width as i32, size.height as i32);

		let mut x = position.x;
		let mut y = position.y;

		if x + width > work_area.right() {
			x = work_area.right() - width;
		}

		if y + height > work_area.bottom() {
			y = work_area.bottom() - height;
		}

		x = x.max(work_area.x);
		y = y.max(work_area.y);

		if x != position.x || y != position.y {
			winit_window.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
		}
	});
}

fn work_area_and_decoration(ui: &MainWindow) -> Option<(Rect, i32)> {
	ui.window().with_winit_window(|winit_window| {
		let monitor = winit_window.current_monitor().map(|monitor| {
			let position = monitor.position();
			let size = monitor.size();

			Rect {
				x: position.x,
				y: position.y,
				width: size.width as i32,
				height: size.height as i32
			}
		});

		let work_area = platform_work_area(winit_window, monitor)?;

		let scale_factor = winit_window.scale_factor();
		let mut decoration_height = (winit_window.outer_size().height as i32 - winit_window.inner_size().height as i32).max(0);

		if decoration_height == 0 && winit_window.is_decorated() {
			decoration_height = (32.0 * scale_factor).round() as i32;
		}

		Some((work_area, decoration_height))
	}).flatten()
}

#[cfg(target_os = "windows")]
fn platform_work_area(winit_window: &winit::window::Window, monitor: Option<Rect>) -> Option<Rect> {
	use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
	use winapi::um::winuser::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};

	let Ok(RawWindowHandle::Win32(handle)) = winit_window.window_handle().map(|handle| handle.as_raw()) else { return monitor };

	unsafe {
		let hmonitor = MonitorFromWindow(handle.hwnd.get() as winapi::shared::windef::HWND, MONITOR_DEFAULTTONEAREST);
		if hmonitor.is_null() {
			return monitor;
		}

		let mut info: MONITORINFO = std::mem::zeroed();
		info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
		if GetMonitorInfoW(hmonitor, &mut info) == 0 {
			return monitor;
		}

		let work = info.rcWork;
		Some(
			Rect {
				x: work.left,
				y: work.top,
				width: (work.right - work.left),
				height: (work.bottom - work.top)
			}
		)
	}
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_work_area(winit_window: &winit::window::Window, monitor: Option<Rect>) -> Option<Rect> {
	use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

	let is_x11 = matches!(winit_window.window_handle().map(|handle| handle.as_raw()), Ok(RawWindowHandle::Xlib(_)) | Ok(RawWindowHandle::Xcb(_)));
	if !is_x11 {
		return monitor;
	}

	match (x11_net_workarea(), monitor) {
		(Some(work_area), Some(monitor)) => {
			work_area.intersect(&monitor).or(Some(monitor))
		},
		(Some(work_area), None) => {
			Some(work_area)
		},
		(None, monitor) => {
			monitor
		}
	}
}

#[cfg(all(unix, not(target_os = "macos")))]
fn x11_net_workarea() -> Option<Rect> {
	use x11_dl::xlib;

	let xlib = xlib::Xlib::open().ok()?;
	unsafe {
		let display = (xlib.XOpenDisplay)(std::ptr::null());
		if display.is_null() {
			return None;
		}

		let mut result = None;
		let atom = (xlib.XInternAtom)(display, c"_NET_WORKAREA".as_ptr(), xlib::True);
		if atom != 0 {
			let root = (xlib.XDefaultRootWindow)(display);
			let mut actual_type: xlib::Atom = 0;
			let mut actual_format: std::os::raw::c_int = 0;
			let mut item_count: std::os::raw::c_ulong = 0;
			let mut bytes_after: std::os::raw::c_ulong = 0;
			let mut data: *mut std::os::raw::c_uchar = std::ptr::null_mut();

			let status = (xlib.XGetWindowProperty)(display, root, atom, 0, 4, xlib::False, xlib::XA_CARDINAL, &mut actual_type, &mut actual_format, &mut item_count, &mut bytes_after, &mut data);

			if status == 0 && !data.is_null() && actual_format == 32 && item_count >= 4 {
				let values = std::slice::from_raw_parts(data as *const std::os::raw::c_long, 4);
				if values[2] > 0 && values[3] > 0 {
					result = Some(
						Rect {
							x: values[0] as i32,
							y: values[1] as i32,
							width: values[2] as i32,
							height: values[3] as i32
						}
					);
				}
			}

			if !data.is_null() {
				(xlib.XFree)(data as *mut std::os::raw::c_void);
			}
		}

		(xlib.XCloseDisplay)(display);
		result
	}
}

#[cfg(target_os = "macos")]
fn platform_work_area(winit_window: &winit::window::Window, monitor: Option<Rect>) -> Option<Rect> {
	let scale_factor = winit_window.scale_factor();

	monitor.map(|monitor| {
		let top = (25.0 * scale_factor).round() as i32;
		let bottom = (80.0 * scale_factor).round() as i32;

		Rect {
			x: monitor.x,
			y: (monitor.y + top),
			width: monitor.width,
			height: (monitor.height - top - bottom).max(1)
		}
	})
}

#[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
fn platform_work_area(_winit_window: &winit::window::Window, monitor: Option<Rect>) -> Option<Rect> {
	monitor
}
