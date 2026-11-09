fn main() {
	#[cfg(target_os = "windows")]
	winresource::WindowsResource::new()
		.set("ProductName", "WebTV MAME Launcher")
		.set("FileDescription", "WebTV MAME Launcher")
		.set("LegalCopyright", "Unlicensed")
		.set_icon("ui/images/icon.ico")
		.compile()
			.expect("Failed to run the Windows resource compiler (rc.exe)");

	let config= slint_build::CompilerConfiguration::new()
		.with_style("fluent-dark".into())
		.embed_resources(slint_build::EmbedResourcesKind::EmbedFiles);

	slint_build::compile_with_config("ui/mainwindow.slint", config).unwrap();

	// `webtv_bridge` is set when the built-in WebTV Bridge is compiled in (the "webtv-bridge" feature).
	println!("cargo::rustc-check-cfg=cfg(webtv_bridge)");

	#[cfg(feature = "webtv-bridge")]
	{
		webtv_bridge::build();
		println!("cargo:rustc-cfg=webtv_bridge");
	}
}

// Builds the real webtv-bridge firmware (3rdparty/webtv-bridge, unchanged) as a shared library the launcher embeds
// (OUT_DIR/webtv_bridge.bin, a .so, .dylib or .dll for the target). It's the firmware's emulator (sim) build, with
// sim_lib.js and the simulated radio replaced by host calls (src/webtvbridge/bridge_host.c). A shared library because
// the firmware keeps its state in globals: each power-on loads a fresh copy (see src/webtvbridge.rs).
//
// Needs a C17 compiler with C11 atomics: GCC or Clang (on Windows, Clang: https://llvm.org, or set CC).
#[cfg(feature = "webtv-bridge")]
mod webtv_bridge {
	use std::{env, fmt::Write as _, fs, path::{Path, PathBuf}, process::Command};

	const FIRMWARE_SOURCES: &[&str] = &[
		"link", "portglue", "web", "modem", "ppp", "gw", "bridge", "settings", "console", "log", "scanlist"
	];

	// What webtvbridge.rs looks up in the library (the Windows export list).
	const EXPORTS: &[&str] = &[
		"bridge_set_host", "bridge_set_time", "bridge_read_events", "bridge_event_count", "bridge_seed_events",
		"bridge_get_state", "bridge_reboot_pending", "bridge_engine_config", "bridge_engine_generation",
		"bridge_diag_output_deadline", "sim_init", "sim_box", "sim_tick", "sim_core0", "sim_mode", "sim_state",
		"port_diag_tx_idle"
	];

	fn write_if_changed(path: &Path, text: &str) {
		if fs::read_to_string(path).ok().as_deref() != Some(text) {
			fs::write(path, text).expect("Couldn't write a generated WebTV Bridge source");
		}
	}

	// The C compiler: CC (or CC_<target>) when set; otherwise clang on Windows (MSVC's cl can't build the firmware)
	// and the cc crate's choice elsewhere.
	fn compiler(target_os: &str, target: &str) -> Command {
		let configured = env::var(format!("CC_{}", target.replace('-', "_"))).or_else(|_| env::var("CC")).ok();
		if target_os == "windows" {
			let clang = configured.unwrap_or_else(|| {
				let installed = PathBuf::from(r"C:\Program Files\LLVM\bin\clang.exe");
				if installed.exists() { installed.display().to_string() } else { "clang".into() }
			});
			let mut command = Command::new(clang);
			command.arg(format!("--target={}", target));
			command
		} else {
			cc::Build::new().cargo_metadata(false).get_compiler().to_command()
		}
	}

	pub fn build() {
		let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
		let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
		let target = env::var("TARGET").unwrap();

		let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
		let upstream = root.join("3rdparty/webtv-bridge/pico");
		let src = upstream.join("src");
		let sim = upstream.join("sim");
		let adapter = root.join("src/webtvbridge");
		let out = PathBuf::from(env::var("OUT_DIR").unwrap());
		let generated = out.join("webtv-bridge");
		fs::create_dir_all(&generated).unwrap();

		println!("cargo:rerun-if-changed={}", upstream.display());
		println!("cargo:rerun-if-changed={}", adapter.display());
		println!("cargo:rerun-if-env-changed=CC");

		// Private copy of port.c with the local handshake rules appended (they need port.c's statics).
		let port_adapter = generated.join("port_adapter.c");
		let rules = fs::read_to_string(adapter.join("bridge_rules.inc")).unwrap();
		let port_c = src.join("port.c").display().to_string().replace('\\', "/");
		write_if_changed(&port_adapter, &format!("#include \"{}\"\n{}", port_c, rules));

		// Keep net_sim.c's TCP section; its radio section is replaced by bridge_host.c.
		let net_sim = fs::read_to_string(sim.join("net_sim.c")).unwrap();
		let tcp_marker = "// ---------------------------------------------------------------- TCP";
		let radio_marker = "// ---------------------------------------------------------------- the radio";
		if net_sim.matches(tcp_marker).count() != 1 || !net_sim.contains(radio_marker) {
			panic!("Unsupported upstream net_sim.c layout (TCP section not found)");
		}
		let net_tcp = net_sim.split(radio_marker).next().unwrap().to_owned()
			+ tcp_marker + net_sim.split(tcp_marker).nth(1).unwrap();
		let net_tcp_path = generated.join("net_tcp.c");
		write_if_changed(&net_tcp_path, &net_tcp.replace("(simulated radio)", "(host uplink)"));

		// The patch table (upstream's sb_patches.S / sb_patches_embed.c) as a plain C array, for any C compiler.
		let patches = fs::read(src.join("gen/sb_patches.bin")).unwrap();
		let mut patch_table = String::with_capacity(patches.len() * 4 + 256);
		patch_table.push_str("// Generated by build.rs from 3rdparty/webtv-bridge/pico/src/gen/sb_patches.bin\n#include <stdint.h>\n");
		patch_table.push_str("_Alignas(4) const uint8_t sb_patches[] = {\n");
		for line in patches.chunks(32) {
			for byte in line {
				let _ = write!(patch_table, "{},", byte);
			}
			patch_table.push('\n');
		}
		patch_table.push_str("};\nconst uint8_t sb_patches_end[1];\n");
		let patch_table_path = generated.join("sb_patches.c");
		write_if_changed(&patch_table_path, &patch_table);

		let mut sources: Vec<PathBuf> = vec![
			sim.join("main_sim.c"),
			adapter.join("bridge_host.c"),
			port_adapter,
			net_tcp_path,
			patch_table_path,
			src.join("gen/sb_assets.c"),
			src.join("gen/web_page.c")
		];
		sources.extend(FIRMWARE_SOURCES.iter().map(|name| src.join(name.to_string() + ".c")));
		let mut startup: Vec<PathBuf> = fs::read_dir(src.join("sb")).unwrap()
			.filter_map(|entry| entry.ok().map(|entry| entry.path()))
			.filter(|path| path.extension().is_some_and(|extension| extension == "c"))
			.collect();
		startup.sort();
		sources.extend(startup);

		let target_library = out.join("webtv_bridge.bin");
		let mut command = compiler(&target_os, &target);
		command
			.args(["-O2", "-std=gnu17", "-DLINK_HOST_SIM", "-U_FORTIFY_SOURCE"])
			.args(["-Wall", "-Wextra", "-Wno-unused-parameter"])
			.arg(format!("-I{}", src.display()))
			.arg(format!("-I{}", adapter.display()))
			.arg("-include").arg(adapter.join("bridge_stdio.h"));

		match target_os.as_str() {
			"windows" => {
				let mut definitions = String::from("EXPORTS\n");
				for name in EXPORTS {
					definitions.push_str(name);
					definitions.push('\n');
				}
				let definitions_path = generated.join("webtv_bridge.def");
				write_if_changed(&definitions_path, &definitions);
				command.arg("-shared");
				if target_env == "msvc" {
					command.arg("-fuse-ld=lld").arg(format!("-Wl,/DEF:{}", definitions_path.display()));
				} else {
					command.arg(&definitions_path);
				}
			},
			"macos" => {
				command.args(["-dynamiclib", "-fPIC"]);
			},
			_ => {
				command.args(["-shared", "-fPIC", "-Wl,-z,defs"]);
			}
		}

		command.args(&sources).arg("-o").arg(&target_library);

		let status = command.status().unwrap_or_else(|error| panic!(
			"Couldn't run the C compiler for the WebTV Bridge ({error}). It needs GCC or Clang (on Windows, Clang from https://llvm.org). \
			Set CC to one, or build without the bridge: cargo build --no-default-features"
		));
		if !status.success() {
			panic!(
				"Couldn't build the WebTV Bridge firmware. It needs GCC or Clang (on Windows, Clang). \
				Set CC to one, or build without the bridge: cargo build --no-default-features"
			);
		}
	}
}
