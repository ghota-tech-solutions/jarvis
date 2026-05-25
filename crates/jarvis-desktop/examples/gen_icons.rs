//! Icon generator.
//!
//! Rasterises `icons/source/jarvis.svg` into the full multi-resolution
//! icon set Tauri's bundler expects (PNG ladder + .ico + .icns). Pure
//! Rust — no external CLI tools (no ImageMagick / cargo-tauri-icon).
//!
//! Run from the crate root:
//!   cargo run --example gen_icons -p jarvis-desktop
//!
//! Outputs (all under `icons/`):
//!   32x32.png 128x128.png 128x128@2x.png icon.png
//!   Square30x30Logo.png Square44x44Logo.png Square71x71Logo.png
//!   Square89x89Logo.png Square107x107Logo.png Square142x142Logo.png
//!   Square150x150Logo.png Square284x284Logo.png Square310x310Logo.png
//!   StoreLogo.png  icon.ico  icon.icns  tray.png

use std::fs;
use std::path::{Path, PathBuf};

use image::{ImageBuffer, Rgba, RgbaImage};
use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{Options, Tree};

const MASTER_SIZE: u32 = 1024;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let svg_path = crate_root.join("icons/source/jarvis.svg");
    let icons_dir = crate_root.join("icons");
    fs::create_dir_all(&icons_dir)?;

    println!(
        "rendering {} -> {}x{} master",
        svg_path.display(),
        MASTER_SIZE,
        MASTER_SIZE
    );
    let master = render_svg(&svg_path, MASTER_SIZE)?;

    // ---- PNG ladder used by Tauri / OS shells ----
    let png_targets: &[(&str, u32)] = &[
        ("32x32.png", 32),
        ("128x128.png", 128),
        ("128x128@2x.png", 256),
        ("icon.png", 512),
        ("tray.png", 64),
        // Windows Store / MSIX sizes (cargo tauri icon also emits these)
        ("Square30x30Logo.png", 30),
        ("Square44x44Logo.png", 44),
        ("Square71x71Logo.png", 71),
        ("Square89x89Logo.png", 89),
        ("Square107x107Logo.png", 107),
        ("Square142x142Logo.png", 142),
        ("Square150x150Logo.png", 150),
        ("Square284x284Logo.png", 284),
        ("Square310x310Logo.png", 310),
        ("StoreLogo.png", 50),
    ];
    for (name, size) in png_targets {
        let out = icons_dir.join(name);
        let resized = resize(&master, *size);
        resized.save(&out)?;
        println!(
            "  wrote {} ({} bytes)",
            out.display(),
            fs::metadata(&out)?.len()
        );
    }

    // ---- Multi-resolution Windows ICO ----
    let ico_path = icons_dir.join("icon.ico");
    write_ico(&master, &ico_path)?;
    println!(
        "  wrote {} ({} bytes)",
        ico_path.display(),
        fs::metadata(&ico_path)?.len()
    );

    // ---- macOS ICNS ----
    let icns_path = icons_dir.join("icon.icns");
    write_icns(&master, &icns_path)?;
    println!(
        "  wrote {} ({} bytes)",
        icns_path.display(),
        fs::metadata(&icns_path)?.len()
    );

    println!("done.");
    Ok(())
}

/// Rasterise the SVG into an RGBA buffer at the requested square size.
fn render_svg(path: &Path, size: u32) -> Result<RgbaImage, Box<dyn std::error::Error>> {
    let svg_data = fs::read(path)?;
    let opt = Options::default();
    let tree = Tree::from_data(&svg_data, &opt)?;
    let svg_size = tree.size();
    let sx = size as f32 / svg_size.width();
    let sy = size as f32 / svg_size.height();
    let mut pixmap = Pixmap::new(size, size).ok_or("pixmap alloc failed")?;
    resvg::render(&tree, Transform::from_scale(sx, sy), &mut pixmap.as_mut());
    let img = ImageBuffer::<Rgba<u8>, _>::from_raw(size, size, pixmap.data().to_vec())
        .ok_or("buffer build failed")?;
    Ok(img)
}

/// High-quality downscale (Lanczos3) of an existing RGBA buffer.
fn resize(src: &RgbaImage, size: u32) -> RgbaImage {
    image::imageops::resize(src, size, size, image::imageops::FilterType::Lanczos3)
}

fn write_ico(master: &RgbaImage, out: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut ico = ico::IconDir::new(ico::ResourceType::Icon);
    for size in [16u32, 24, 32, 48, 64, 128, 256] {
        let img = resize(master, size);
        let entry = ico::IconImage::from_rgba_data(size, size, img.into_raw());
        ico.add_entry(ico::IconDirEntry::encode(&entry)?);
    }
    let file = fs::File::create(out)?;
    ico.write(file)?;
    Ok(())
}

fn write_icns(master: &RgbaImage, out: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut family = icns::IconFamily::new();
    // (icns OSType, square pixel size). The `icns` 0.3 crate caps at 512.
    let targets: &[(icns::IconType, u32)] = &[
        (icns::IconType::RGBA32_16x16, 16),
        (icns::IconType::RGBA32_32x32, 32),
        (icns::IconType::RGBA32_64x64, 64),
        (icns::IconType::RGBA32_128x128, 128),
        (icns::IconType::RGBA32_256x256, 256),
        (icns::IconType::RGBA32_512x512, 512),
    ];
    for (kind, size) in targets {
        let img = resize(master, *size);
        let icns_img =
            icns::Image::from_data(icns::PixelFormat::RGBA, *size, *size, img.into_raw())?;
        family.add_icon_with_type(&icns_img, *kind)?;
    }
    let file = fs::File::create(out)?;
    family.write(file)?;
    Ok(())
}
