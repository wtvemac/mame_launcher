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
}
