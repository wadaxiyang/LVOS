#[cfg(target_os = "windows")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=../desktop/resources/lvos.ico");
    winresource::WindowsResource::new()
        .set_icon("../desktop/resources/lvos.ico")
        .compile()?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn main() {
    println!("cargo:rerun-if-changed=../desktop/resources/lvos.ico");
}
