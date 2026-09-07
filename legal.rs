// Shared by the host and panel; opening notices needs neither Kindle nor the pipe.
use std::io;
use std::path::PathBuf;

pub fn open() -> io::Result<()> {
    let installed = std::env::current_exe()?.with_file_name("legal.html");
    let path = if installed.is_file() {
        installed
    } else if cfg!(debug_assertions) {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../legal.html")
    } else {
        return Err(io::Error::new(io::ErrorKind::NotFound, "Installed legal notices are missing"));
    };
    if !path.is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "Legal notices are missing"));
    }
    // Pass the path as an argument, without a command shell. Explorer opens HTML in
    // the user's default browser and does not flash a console from either GUI exe.
    std::process::Command::new("explorer.exe").arg(path).spawn()?;
    Ok(())
}
