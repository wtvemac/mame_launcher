// By: Eric MacDonald (eMac)

use std::cell::Cell;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model};
use slint::winit_030::{winit, EventResult, WinitWindowAccessor};

use crate::{ComboBoxWindow, HintedItem, MainWindow, PopupListWindow};

thread_local! {
	static CREATING_POPUP_WINDOW: Cell<bool> = const { Cell::new(false) };
}
const REOPEN_GUARD: Duration = Duration::from_millis(300);
const FOCUS_CHECK_DELAY: Duration = Duration::from_millis(150);
const SETUP_RETRY_DELAY: Duration = Duration::from_millis(100);
const SETUP_RETRY_COUNT: u32 = 50;

struct ExternalPopup {
	main: slint::Weak<MainWindow>,
	popup: PopupListWindow,
	is_open: Cell<bool>,
	popup_events_hooked: Cell<bool>,
	closed_by_main_press: Cell<Option<Instant>>
}

pub fn window_attributes_hook(attributes: winit::window::WindowAttributes) -> winit::window::WindowAttributes {
	if !CREATING_POPUP_WINDOW.with(|creating| creating.get()) {
		return attributes;
	}

	#[allow(unused_mut)]
	let mut attributes = attributes
		.with_decorations(false)
		.with_resizable(false)
		// Don't take focus away from the main window when shown.
		.with_active(false)
		.with_window_level(winit::window::WindowLevel::AlwaysOnTop);

	// Mark it as a dropdown and keep the window manager from managing it (tiling, placing etc...). This also keeps it from taking focus.
	#[cfg(all(unix, not(target_os = "macos")))]
	{
		use winit::platform::x11::{WindowAttributesExtX11, WindowType};

		attributes = attributes
			.with_x11_window_type(vec![WindowType::DropdownMenu])
			.with_override_redirect(true);
	}

	// Keep it out of the taskbar.
	#[cfg(target_os = "windows")]
	{
		use winit::platform::windows::WindowAttributesExtWindows;

		attributes = attributes.with_skip_taskbar(true);
	}

	attributes
}

pub fn setup(ui: &MainWindow) -> Result<(), Box<dyn std::error::Error>> {
	let state = Rc::new(ExternalPopup {
		main: ui.as_weak(),
		popup: {
			CREATING_POPUP_WINDOW.with(|creating| creating.set(true));
			let popup = PopupListWindow::new();
			CREATING_POPUP_WINDOW.with(|creating| creating.set(false));
			popup?
		},
		is_open: Cell::new(false),
		popup_events_hooked: Cell::new(false),
		closed_by_main_press: Cell::new(None)
	});

	let state_weak = Rc::downgrade(&state);
	state.popup.on_item_selected(move |index| {
		if let Some(state) = state_weak.upgrade() {
			state.close(index);
		}
	});

	let state_strong = state.clone();
	ui.global::<ComboBoxWindow>().on_show_external_popup(move |x, anchor_top, anchor_bottom, width, height, row_height, items, current_value, hint_before, display_hint_only| {
		state_strong.show(x, anchor_top, anchor_bottom, width, height, row_height, items, current_value, hint_before, display_hint_only);
	});

	connect_close_callbacks(ui, &state);

	detect_support(Rc::downgrade(&state), SETUP_RETRY_COUNT);

	Ok(())
}

fn detect_support(state_weak: Weak<ExternalPopup>, retries_left: u32) {
	slint::Timer::single_shot(if retries_left == SETUP_RETRY_COUNT { Duration::ZERO } else { SETUP_RETRY_DELAY }, move || {
		let Some(state) = state_weak.upgrade() else { return };
		let Some(main) = state.main.upgrade() else { return };

		// None when the main window doesn't exist yet (or isn't a winit window).
		let supported = main.window().with_winit_window(|winit_window| {
			use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

			!matches!(winit_window.window_handle().map(|handle| handle.as_raw()), Ok(RawWindowHandle::Wayland(_)))
		});

		match supported {
			Some(supported) => {
				main.global::<ComboBoxWindow>().set_external_popups_supported(supported);

				if supported {
					let state_weak = Rc::downgrade(&state);
					main.window().on_winit_window_event(move |_window, event| {
						crate::mamecapture::on_main_window_event(event);

						if let Some(state) = state_weak.upgrade() {
							state.on_main_window_event(event);
						}
						EventResult::Propagate
					});
				}
			},
			None => {
				if retries_left > 0 {
					detect_support(Rc::downgrade(&state), retries_left - 1);
				}
			}
		}
	});
}

impl ExternalPopup {
	#[allow(clippy::too_many_arguments)]
	fn show(&self, x: f32, anchor_top: f32, anchor_bottom: f32, width: f32, height: f32, row_height: f32, items: slint::ModelRc<HintedItem>, current_value: slint::SharedString, hint_before: bool, display_hint_only: bool) {
		// Clicking the dropdown arrow while open toggles it closed.
		if self.is_open.get() {
			self.close(-1);
			return;
		}
		if let Some(closed_at) = self.closed_by_main_press.take() {
			if closed_at.elapsed() < REOPEN_GUARD {
				self.finish(-1);
				return;
			}
		}

		let Some(main) = self.main.upgrade() else { return };

		let scale_factor = main.window().scale_factor();
		let placement = main.window().with_winit_window(|winit_window| {
			let content_origin = winit_window.inner_position().ok()?;
			let monitor = winit_window.current_monitor().map(|monitor| (monitor.position(), monitor.size()));
			Some((content_origin, monitor))
		}).flatten();

		let Some((content_origin, monitor)) = placement else {
			// Can't place the window, use the in-window dropdown from now on. The next click opens that one.
			main.global::<ComboBoxWindow>().set_external_popups_supported(false);
			self.finish(-1);
			return;
		};

		let to_physical = |length: f32| (length * scale_factor).round() as i32;

		let popup_width = to_physical(width).max(1);
		let mut popup_height = to_physical(height).max(1);
		let mut popup_x = content_origin.x + to_physical(x);
		let below_y = content_origin.y + to_physical(anchor_bottom);
		let above_bottom_y = content_origin.y + to_physical(anchor_top);
		let mut popup_y = below_y;

		// Open below unless it doesn't fit on the monitor and there's more room above. Shorten it to fit either way (the list scrolls).
		if let Some((monitor_position, monitor_size)) = monitor {
			let monitor_top = monitor_position.y;
			let monitor_bottom = monitor_position.y + monitor_size.height as i32;
			let monitor_left = monitor_position.x;
			let monitor_right = monitor_position.x + monitor_size.width as i32;

			let space_below = monitor_bottom - below_y;
			let space_above = above_bottom_y - monitor_top;
			let min_height = to_physical(row_height).max(1);

			if popup_height > space_below && space_above > space_below {
				popup_height = popup_height.min(space_above).max(min_height);
				popup_y = above_bottom_y - popup_height;
			} else {
				popup_height = popup_height.min(space_below).max(min_height);
			}

			popup_x = popup_x.min(monitor_right - popup_width).max(monitor_left);
		}

		let selected_index = items.iter().position(|item| item.value == current_value).map_or(-1, |index| index as i32);

		self.popup.set_items(items);
		self.popup.set_current_value(current_value);
		self.popup.set_hint_before(hint_before);
		self.popup.set_display_hint_only(display_hint_only);
		self.popup.set_row_height(row_height);

		let popup_window = self.popup.window();
		popup_window.set_size(slint::PhysicalSize::new(popup_width as u32, popup_height as u32));
		popup_window.set_position(slint::PhysicalPosition::new(popup_x, popup_y));

		if self.popup.show().is_err() {
			main.global::<ComboBoxWindow>().set_external_popups_supported(false);
			self.finish(-1);
			return;
		}
		self.is_open.set(true);

		self.popup.invoke_scroll_to_index(selected_index, popup_height as f32 / scale_factor);

		// Some platforms ignore the position before the window exists.
		popup_window.set_position(slint::PhysicalPosition::new(popup_x, popup_y));

		self.hook_popup_events();
	}

	fn hook_popup_events(&self) {
		if self.popup_events_hooked.get() || !self.popup.window().has_winit_window() {
			return;
		}
		self.popup_events_hooked.set(true);

		let main_weak = self.main.clone();
		self.popup.window().on_winit_window_event(move |_window, event| {
			let close = match event {
				winit::event::WindowEvent::Focused(false) => {
					// If focus went to another app (not the main window or this list), close.
					let main_weak = main_weak.clone();
					slint::Timer::single_shot(FOCUS_CHECK_DELAY, move || {
						if let Some(main) = main_weak.upgrade() {
							main.global::<ComboBoxWindow>().invoke_close_external_popup_if_unfocused();
						}
					});
					false
				},
				winit::event::WindowEvent::KeyboardInput { event, .. } => {
					event.state == winit::event::ElementState::Pressed && event.logical_key == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
				},
				_ => false
			};
			if close {
				if let Some(main) = main_weak.upgrade() {
					main.global::<ComboBoxWindow>().invoke_close_external_popup();
				}
			}
			EventResult::Propagate
		});
	}

	fn on_main_window_event(&self, event: &winit::event::WindowEvent) {
		if !self.is_open.get() {
			return;
		}

		match event {
			// Clicking anywhere in the main window closes the list (like the in-window dropdown).
			winit::event::WindowEvent::MouseInput { state: winit::event::ElementState::Pressed, .. } => {
				self.close(-1);
				self.closed_by_main_press.set(Some(Instant::now()));
			},
			winit::event::WindowEvent::KeyboardInput { event, .. } => {
				if event.state == winit::event::ElementState::Pressed && event.logical_key == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape) {
					self.close(-1);
				}
			},
			// The list would be left behind if the main window moves, resizes or closes.
			winit::event::WindowEvent::Moved(_)
			| winit::event::WindowEvent::Resized(_)
			| winit::event::WindowEvent::CloseRequested
			| winit::event::WindowEvent::Occluded(true) => {
				self.close(-1);
			},
			winit::event::WindowEvent::Focused(false) => {
				let main_weak = self.main.clone();
				slint::Timer::single_shot(FOCUS_CHECK_DELAY, move || {
					if let Some(main) = main_weak.upgrade() {
						main.global::<ComboBoxWindow>().invoke_close_external_popup_if_unfocused();
					}
				});
			},
			_ => {}
		}
	}

	fn close_if_unfocused(&self) {
		if !self.is_open.get() {
			return;
		}

		let has_focus = |window: &slint::Window| window.with_winit_window(|winit_window| winit_window.has_focus()).unwrap_or(false);
		let main_has_focus = self.main.upgrade().map(|main| has_focus(main.window())).unwrap_or(false);

		if !main_has_focus && !has_focus(self.popup.window()) {
			self.close(-1);
		}
	}

	fn close(&self, selected_index: i32) {
		if self.is_open.replace(false) {
			let _ = self.popup.hide();
		}
		self.finish(selected_index);
	}

	fn finish(&self, selected_index: i32) {
		if let Some(main) = self.main.upgrade() {
			let combobox_window = main.global::<ComboBoxWindow>();
			combobox_window.set_external_popup_selected_index(selected_index);
			combobox_window.set_external_popup_serial(combobox_window.get_external_popup_serial().wrapping_add(1));
		}
	}
}

fn connect_close_callbacks(ui: &MainWindow, state: &Rc<ExternalPopup>) {
	let state_weak = Rc::downgrade(state);
	ui.global::<ComboBoxWindow>().on_close_external_popup(move || {
		if let Some(state) = state_weak.upgrade() {
			state.close(-1);
		}
	});
	let state_weak = Rc::downgrade(state);
	ui.global::<ComboBoxWindow>().on_close_external_popup_if_unfocused(move || {
		if let Some(state) = state_weak.upgrade() {
			state.close_if_unfocused();
		}
	});
}
