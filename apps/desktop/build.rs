fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=ui/app.slint");
    slint_build::compile("ui/app.slint")?;
    Ok(())
}
