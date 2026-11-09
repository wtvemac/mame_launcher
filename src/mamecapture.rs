// By: Eric MacDonald (eMac)

use std::cell::RefCell;
use std::time::Duration;

use slint::ComponentHandle;
use slint::winit_030::{winit, WinitWindowAccessor};

use crate::{MainWindow, UIMAMEOptions};

// The main window may not exist right when the event loop starts.
const SETUP_RETRY_DELAY: Duration = Duration::from_millis(100);
const SETUP_RETRY_COUNT: u32 = 50;

thread_local! {
	static HOST: RefCell<Option<HostWindow>> = const { RefCell::new(None) };
}

pub fn setup(ui: &MainWindow) {
	detect_support(ui.as_weak(), SETUP_RETRY_COUNT);
}

fn detect_support(ui_weak: slint::Weak<MainWindow>, retries_left: u32) {
	slint::Timer::single_shot(if retries_left == SETUP_RETRY_COUNT { Duration::ZERO } else { SETUP_RETRY_DELAY }, move || {
		let Some(ui) = ui_weak.upgrade() else { return };

		match parent_handle(&ui) {
			Some(parent) => {
				ui.global::<UIMAMEOptions>().set_capture_platform_supported(parent.is_some());
			},
			None => {
				if retries_left > 0 {
					detect_support(ui_weak, retries_left - 1);
				}
			}
		}
	});
}

fn parent_handle(ui: &MainWindow) -> Option<Option<u64>> {
	ui.window().with_winit_window(|winit_window| {
		use winit::raw_window_handle::HasWindowHandle;
		#[cfg(any(all(unix, not(target_os = "macos")), target_os = "windows"))]
		use winit::raw_window_handle::RawWindowHandle;

		match winit_window.window_handle().map(|handle| handle.as_raw()) {
			#[cfg(all(unix, not(target_os = "macos")))]
			Ok(RawWindowHandle::Xlib(handle)) => Some(handle.window as u64),
			#[cfg(all(unix, not(target_os = "macos")))]
			Ok(RawWindowHandle::Xcb(handle)) => Some(handle.window.get() as u64),
			#[cfg(target_os = "windows")]
			Ok(RawWindowHandle::Win32(handle)) => Some(handle.hwnd.get() as u64),
			_ => None
		}
	})
}

pub fn start(ui: &MainWindow) -> Option<String> {
	stop();

	let parent = parent_handle(ui).flatten()?;
	let host = HostWindow::create(parent)?;
	let host_id = host.id().to_string();

	HOST.with(|host_window| *host_window.borrow_mut() = Some(host));
	update_geometry(ui);

	Some(host_id)
}

pub fn update_geometry(ui: &MainWindow) {
	let scale_factor = ui.window().scale_factor();
	let to_physical = |length: f32| (length * scale_factor).round().max(0.0) as i32;

	let x = to_physical(ui.get_mame_area_x());
	let y = to_physical(ui.get_mame_area_y());
	let width = to_physical(ui.get_mame_area_width());
	let height = to_physical(ui.get_mame_area_height());

	HOST.with(|host_window| {
		if let Some(host) = host_window.borrow().as_ref() {
			host.set_geometry(x, y, width, height);
		}
	});
}

pub fn stop() {
	// Removes the host window (wtvmame has exited)
	HOST.with(|host_window| {
		if let Some(host) = host_window.borrow_mut().take() {
			host.focus_parent();
			host.destroy();
		}
	});
}

pub fn on_main_window_event(event: &winit::event::WindowEvent) {
	if let winit::event::WindowEvent::MouseInput { state: winit::event::ElementState::Pressed, .. } = event {
		HOST.with(|host_window| {
			if let Some(host) = host_window.borrow().as_ref() {
				host.focus_parent();
			}
		});
	}
}

// X11 stuff

#[cfg(all(unix, not(target_os = "macos")))]
struct HostWindow {
	xlib: x11_dl::xlib::Xlib,
	display: *mut x11_dl::xlib::Display,
	parent: u64,
	window: u64
}

#[cfg(all(unix, not(target_os = "macos")))]
impl HostWindow {
	fn create(parent: u64) -> Option<HostWindow> {
		let xlib = x11_dl::xlib::Xlib::open().ok()?;

		unsafe {
			let display = (xlib.XOpenDisplay)(std::ptr::null());
			if display.is_null() {
				return None;
			}

			// Opaque black for the window's pixel format so we don't see windows underneath as you resize the launcher window
			let opaque_black = |window: std::os::raw::c_ulong| -> std::os::raw::c_ulong {
				let mut attributes: x11_dl::xlib::XWindowAttributes = std::mem::zeroed();
				if (xlib.XGetWindowAttributes)(display, window, &mut attributes) != 0 && attributes.depth == 32 {
					0xff000000
				} else {
					(xlib.XBlackPixel)(display, (xlib.XDefaultScreen)(display))
				}
			};
			let black = opaque_black(parent as std::os::raw::c_ulong);

			let window = (xlib.XCreateSimpleWindow)(display, parent as std::os::raw::c_ulong, 0, 0, 1, 1, 0, black, black);
			if window == 0 {
				(xlib.XCloseDisplay)(display);
				return None;
			}

			// The launcher window too, so parts uncovered by a resize are black until it's redrawn.
			(xlib.XSetWindowBackground)(display, parent as std::os::raw::c_ulong, black);

			(xlib.XMapWindow)(display, window);
			(xlib.XFlush)(display);

			Some(HostWindow { xlib, display, parent, window: window as u64 })
		}
	}

	fn id(&self) -> u64 {
		self.window
	}

	fn set_geometry(&self, x: i32, y: i32, width: i32, height: i32) {
		unsafe {
			(self.xlib.XMoveResizeWindow)(self.display, self.window as std::os::raw::c_ulong, x, y, width.max(1) as u32, height.max(1) as u32);
			(self.xlib.XFlush)(self.display);
		}
	}

	fn focus_parent(&self) {
		unsafe {
			(self.xlib.XSetInputFocus)(self.display, self.parent as std::os::raw::c_ulong, x11_dl::xlib::RevertToParent, x11_dl::xlib::CurrentTime);
			(self.xlib.XFlush)(self.display);
		}
	}

	fn destroy(self) {
		unsafe {
			(self.xlib.XDestroyWindow)(self.display, self.window as std::os::raw::c_ulong);
			(self.xlib.XCloseDisplay)(self.display);
		}
	}
}

// Windows stuff

#[cfg(target_os = "windows")]
struct HostWindow {
	parent: winapi::shared::windef::HWND,
	window: winapi::shared::windef::HWND
}

#[cfg(target_os = "windows")]
impl HostWindow {
	fn create(parent: u64) -> Option<HostWindow> {
		use winapi::um::winuser::{CreateWindowExW, GetWindowLongPtrW, SetWindowLongPtrW, GWL_STYLE, SS_BLACKRECT, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_VISIBLE};

		let parent = parent as usize as winapi::shared::windef::HWND;
		let class_name: Vec<u16> = "STATIC\0".encode_utf16().collect();

		unsafe {
			let parent_style = GetWindowLongPtrW(parent, GWL_STYLE);
			SetWindowLongPtrW(parent, GWL_STYLE, parent_style | WS_CLIPCHILDREN as isize);

			let window = CreateWindowExW(
				0,
				class_name.as_ptr(),
				std::ptr::null(),
				WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | WS_CLIPCHILDREN | SS_BLACKRECT,
				0, 0, 1, 1,
				parent,
				std::ptr::null_mut(),
				winapi::um::libloaderapi::GetModuleHandleW(std::ptr::null()),
				std::ptr::null_mut()
			);
			if window.is_null() {
				return None;
			}

			Some(HostWindow { parent, window })
		}
	}

	fn id(&self) -> u64 {
		self.window as usize as u64
	}

	fn set_geometry(&self, x: i32, y: i32, width: i32, height: i32) {
		use winapi::um::winuser::{SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER};

		unsafe {
			SetWindowPos(self.window, std::ptr::null_mut(), x, y, width.max(1), height.max(1), SWP_NOZORDER | SWP_NOACTIVATE);
		}
	}

	fn focus_parent(&self) {
		unsafe {
			winapi::um::winuser::SetFocus(self.parent);
		}
	}

	fn destroy(self) {
		unsafe {
			winapi::um::winuser::DestroyWindow(self.window);
		}
	}
}

// Everything that isn't supported (macOS)

#[cfg(not(any(all(unix, not(target_os = "macos")), target_os = "windows")))]
struct HostWindow;

#[cfg(not(any(all(unix, not(target_os = "macos")), target_os = "windows")))]
impl HostWindow {
	fn create(_parent: u64) -> Option<HostWindow> {
		None
	}

	fn id(&self) -> u64 {
		0
	}

	fn set_geometry(&self, _x: i32, _y: i32, _width: i32, _height: i32) {}

	fn focus_parent(&self) {}

	fn destroy(self) {}
}
