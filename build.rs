//! Embeds a Windows version resource so the binaries have an identity.
//!
//! Without this, anything that enumerates audio sessions (SteelSeries Sonar,
//! Windows' own per-app volume list, task managers) shows the process as
//! "UNKNOWN", because it has no ProductName or FileDescription to display.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set("ProductName", "raop-sender");
        res.set("FileDescription", "AirPlay 1 / RAOP sender for Windows");
        res.set("CompanyName", "Lyoraxis");
        res.set("LegalCopyright", "MIT");
        res.set("OriginalFilename", "raop-daemon.exe");
        res.set("InternalName", "raop-sender");
        res.set_manifest(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="raop-sender" version="0.1.0.0"/>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>"#,
        );
        res.compile().expect("windows resource");
    }
}
