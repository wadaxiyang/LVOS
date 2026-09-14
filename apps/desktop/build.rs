fn main() -> Result<(), Box<dyn std::error::Error>> {
    compile_ui()
}

#[cfg(not(feature = "ui"))]
fn compile_ui() -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

#[cfg(feature = "ui")]
fn compile_ui() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=ui/app.slint");
    println!("cargo:rerun-if-changed=resources/lvos.ico");
    #[cfg(all(feature = "ui", target_os = "windows"))]
    winresource::WindowsResource::new()
        .set_icon("resources/lvos.ico")
        .compile()?;
    let libraries = std::collections::HashMap::from([(
        quadrant_kit::SLINT_LIBRARY_NAME.to_owned(),
        quadrant_kit::slint_library_path(),
    )]);
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        .with_library_paths(libraries)
        .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles);
    slint_build::compile_with_config("ui/app.slint", config)?;
    Ok(())
}
