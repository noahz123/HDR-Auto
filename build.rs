#[cfg(windows)]
fn main() {
    println!("cargo:rerun-if-changed=icon_tray.png");

    let out_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set"));
    let icon_path = out_dir.join("icon_tray.ico");
    write_png_as_ico("icon_tray.png", &icon_path).expect("icon_tray.png can be converted to ico");

    let mut resource = winres::WindowsResource::new();
    resource.set_icon(
        icon_path
            .to_str()
            .expect("generated icon path should be valid UTF-8"),
    );
    resource
        .compile()
        .expect("Windows resources can be compiled");
}

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn write_png_as_ico(
    png_path: impl AsRef<std::path::Path>,
    ico_path: impl AsRef<std::path::Path>,
) -> std::io::Result<()> {
    let png = std::fs::read(png_path)?;
    let (width, height) = png_dimensions(&png)?;

    let mut ico = Vec::with_capacity(22 + png.len());
    ico.extend_from_slice(&0u16.to_le_bytes());
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.push(icon_dimension_byte(width));
    ico.push(icon_dimension_byte(height));
    ico.push(0);
    ico.push(0);
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&32u16.to_le_bytes());
    ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
    ico.extend_from_slice(&22u32.to_le_bytes());
    ico.extend_from_slice(&png);

    std::fs::write(ico_path, ico)
}

#[cfg(windows)]
fn png_dimensions(png: &[u8]) -> std::io::Result<(u32, u32)> {
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

    if png.len() < 24 || &png[..8] != PNG_SIGNATURE || &png[12..16] != b"IHDR" {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "icon_tray.png is not a valid PNG",
        ));
    }

    let width = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let height = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    if width == 0 || width > 256 || height == 0 || height > 256 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "icon_tray.png dimensions must be between 1 and 256 pixels",
        ));
    }

    Ok((width, height))
}

#[cfg(windows)]
fn icon_dimension_byte(value: u32) -> u8 {
    if value == 256 {
        0
    } else {
        value as u8
    }
}
