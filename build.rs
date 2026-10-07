// Embed the Windows icon and version information in the executable.
fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/appicon/pictureman.ico");
        res.set("ProductName", "Picture Man");
        res.set("FileDescription", "Picture Man image editor");
        res.set(
            "LegalCopyright",
            "Potapov WORKS, STOIK Ltd. 1991-1993; rebuild 2026",
        );
        if let Err(e) = res.compile() {
            println!("cargo:warning=icon not embedded: {e}");
        }
    }
}
