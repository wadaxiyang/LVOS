fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=../desktop/resources/lvos.ico");
    #[cfg(target_os = "windows")]
    winresource::WindowsResource::new()
        .set_icon("../desktop/resources/lvos.ico")
        .compile()?;
    Ok(())
}
