fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(not(target_os = "android"))]
    festerm_mobile::run_native()?;
    Ok(())
}
