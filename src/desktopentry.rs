// By: Eric MacDonald (eMac)

// Linux only

// This adds an app icon using a .desktop file allowing you to see something other than the ! icon in taskbards/docks and menus

use std::{env, fs, path::{Path, PathBuf}};

pub const APP_ID: &str = "com.ericmacd.mame_launcher";
const APP_NAME: &str = "WebTV MAME Launcher";
const ICON_PNG: &[u8] = include_bytes!("../ui/images/icon.png");
const ICON_SIZE: &str = "128x128";

pub fn setup() {
	let _ = slint::set_xdg_app_id(APP_ID);

	if env::var_os("MAME_LAUNCHER_NO_DESKTOP_ENTRY").is_none() {
		let _ = install();
	}
}

fn data_home() -> Option<PathBuf> {
	env::var_os("XDG_DATA_HOME")
		.map(PathBuf::from)
		.filter(|path| path.is_absolute())
		.or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
}

fn exec_quote(path: &Path) -> String {
	let mut quoted = String::from("\"");

	for character in path.to_string_lossy().chars() {
		match character {
			'"' | '`' | '$' => { quoted.push_str("\\\\"); quoted.push(character); },
			'\\' => quoted.push_str("\\\\\\\\"),
			'%' => quoted.push_str("%%"),
			_ => quoted.push(character)
		}
	}

	quoted.push('"');

	quoted
}

fn write_if_changed(path: &Path, contents: &[u8]) -> std::io::Result<bool> {
	if fs::read(path).ok().as_deref() == Some(contents) {
		return Ok(false);
	} else {
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent)?;
		}

		fs::write(path, contents)?;

		Ok(true)
	}
}

fn install() -> std::io::Result<()> {
	let data_home = data_home().ok_or(std::io::ErrorKind::NotFound)?;
	let exe = env::current_exe()?;
	let exe = fs::canonicalize(&exe).unwrap_or(exe);

	let icon_dir = data_home.join("icons/hicolor").join(ICON_SIZE).join("apps");
	let icon_path = icon_dir.join(format!("{APP_ID}.png"));
	let icon_changed = write_if_changed(&icon_path, ICON_PNG)?;

	let entry = format!(
		"[Desktop Entry]\n\
		Type=Application\n\
		Name={APP_NAME}\n\
		Comment=Launch MAME with the WebTV driver\n\
		Exec={}\n\
		Icon={APP_ID}\n\
		Terminal=false\n\
		Categories=Game;Emulator;\n\
		StartupWMClass={APP_ID}\n\
		StartupNotify=true\n",
		exec_quote(&exe)
	);

	let entry_path = data_home.join("applications").join(format!("{APP_ID}.desktop"));
	let entry_changed = write_if_changed(&entry_path, entry.as_bytes())?;

	if icon_changed {
		let hicolor = data_home.join("icons/hicolor");
		let _ = fs::File::open(&hicolor).and_then(|dir| dir.set_modified(std::time::SystemTime::now()));
	}

	if entry_changed {
		let _ = std::process::Command::new("update-desktop-database")
			.arg(data_home.join("applications"))
			.stdout(std::process::Stdio::null())
			.stderr(std::process::Stdio::null())
			.spawn();
	}

	Ok(())
}
