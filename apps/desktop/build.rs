fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=ui/app.slint");
    println!("cargo:rerun-if-changed=resources/lvos.ico");
    #[cfg(target_os = "windows")]
    winresource::WindowsResource::new()
        .set_icon("resources/lvos.ico")
        .compile()?;
    slint_build::compile("ui/app.slint")?;
    Ok(())
}
