//! Album covers: one image per album folder, named after the folder
//! ("Artist - Album (Year).jpg"), shrunk to small thumbnails for the cover wall.

use gpui::RenderImage;
use image::{DynamicImage, Frame, ImageReader, RgbaImage, codecs::jpeg::JpegEncoder};
use std::{
    collections::HashMap,
    fs,
    io::BufWriter,
    path::{Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};

/// Thumbnails are square and come in these sizes, in pixels per side. The
/// wall uses the smallest one at least as big as a tile on screen.
pub const THUMBNAIL_SIZES: [u32; 4] = [128, 256, 512, 1024];

/// The thumbnail size for a tile this many screen pixels wide.
pub fn thumbnail_size(tile_px: f32) -> u32 {
    THUMBNAIL_SIZES
        .into_iter()
        .find(|&size| size as f32 >= tile_px)
        .unwrap_or(THUMBNAIL_SIZES[THUMBNAIL_SIZES.len() - 1])
}

/// `C:\_code\music_backup_v5\AlbumCovers`, unless `AURORA_NG_COVERS` points somewhere else.
pub fn covers_dir() -> PathBuf {
    std::env::var_os("AURORA_NG_COVERS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\_code\music_backup_v5\AlbumCovers"))
}

/// `%LOCALAPPDATA%\aurora_ng\thumbnails`, created if needed.
pub fn thumbnail_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
        .join("aurora_ng")
        .join("thumbnails");
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Every cover image in the covers folder, by lowercase file name without extension.
pub struct CoverIndex {
    by_name: HashMap<String, PathBuf>,
}

impl CoverIndex {
    pub fn scan(dir: &Path) -> Result<Self, String> {
        let entries = fs::read_dir(dir)
            .map_err(|error| format!("Album covers not found at {}: {error}", dir.display()))?;
        let mut by_name = HashMap::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let is_image = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    matches!(
                        extension.to_ascii_lowercase().as_str(),
                        "jpg" | "jpeg" | "png" | "gif" | "bmp"
                    )
                });
            if let (true, Some(stem)) = (is_image, path.file_stem().and_then(|stem| stem.to_str()))
            {
                by_name.entry(stem.to_lowercase()).or_insert(path);
            }
        }
        Ok(Self { by_name })
    }

    /// The cover for an album folder like `D:\MUSIC\Kittie - Oracle (2001)`.
    /// Disc folders (`...\Album (2001)\CD1`) fall back to the album folder.
    pub fn find(&self, folder: &str) -> Option<PathBuf> {
        let mut names = folder
            .split(['\\', '/'])
            .rev()
            .filter(|part| !part.is_empty());
        let lookup = |name: Option<&str>| {
            name.and_then(|name| self.by_name.get(&name.to_lowercase()).cloned())
        };
        lookup(names.next()).or_else(|| lookup(names.next()))
    }
}

/// A square thumbnail of `cover`, `size` pixels per side, ready to draw. Reuses the copy saved in
/// `cache_dir` when there is one, and saves a new one otherwise.
pub fn thumbnail(
    cover: &Path,
    size: u32,
    cache_dir: Option<&Path>,
) -> Result<Arc<RenderImage>, String> {
    let cached = cache_dir.and_then(|dir| Some(dir.join(cache_name(cover, size)?)));
    let pixels = match cached.as_deref().and_then(|path| image::open(path).ok()) {
        Some(image) => image.into_rgba8(),
        None => {
            let image = make_thumbnail(cover, size)?;
            if let Some(path) = &cached {
                save_jpeg(&image, path);
            }
            image.into_rgba8()
        }
    };
    Ok(Arc::new(render_image(pixels)))
}

fn make_thumbnail(cover: &Path, size: u32) -> Result<DynamicImage, String> {
    let image = ImageReader::open(cover)
        .and_then(|reader| reader.with_guessed_format())
        .map_err(|error| format!("Could not open {}: {error}", cover.display()))?
        .decode()
        .map_err(|error| format!("Could not read {}: {error}", cover.display()))?;
    // Crop to the centre square so every tile on the wall lines up.
    let side = image.width().min(image.height());
    let square = image.crop_imm(
        (image.width() - side) / 2,
        (image.height() - side) / 2,
        side,
        side,
    );
    Ok(if side > size {
        square.thumbnail(size, size)
    } else {
        square
    })
}

/// Writes to a temporary name first, so a crash never leaves half a file behind.
fn save_jpeg(image: &DynamicImage, path: &Path) {
    let temporary = path.with_extension("tmp");
    let written = fs::File::create(&temporary).is_ok_and(|file| {
        let encoder = JpegEncoder::new_with_quality(BufWriter::new(file), 85);
        image.to_rgb8().write_with_encoder(encoder).is_ok()
    });
    if !written || fs::rename(&temporary, path).is_err() {
        fs::remove_file(&temporary).ok();
    }
}

/// Named after the cover's path, size and modified time, so replacing a
/// cover makes a new thumbnail.
fn cache_name(cover: &Path, size: u32) -> Option<String> {
    let metadata = fs::metadata(cover).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    let key = format!(
        "{}|{}|{modified}|{size}",
        cover.to_string_lossy().to_lowercase(),
        metadata.len()
    );
    Some(format!("{:016x}.jpg", fnv1a(key.as_bytes())))
}

/// A stable hash: the same input gives the same name in every build.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// GPUI draws images stored as BGRA.
fn render_image(mut pixels: RgbaImage) -> RenderImage {
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    RenderImage::new(vec![Frame::new(pixels)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(names: &[&str]) -> CoverIndex {
        CoverIndex {
            by_name: names
                .iter()
                .map(|name| (name.to_lowercase(), PathBuf::from(format!("{name}.jpg"))))
                .collect(),
        }
    }

    #[test]
    fn finds_covers_by_folder_name() {
        let covers = index(&["Kittie - Oracle (2001)"]);
        assert_eq!(
            covers.find(r"D:\MUSIC\Kittie - Oracle (2001)"),
            Some(PathBuf::from("Kittie - Oracle (2001).jpg"))
        );
        assert_eq!(
            covers.find(r"D:\MUSIC\kittie - oracle (2001)\"),
            Some(PathBuf::from("Kittie - Oracle (2001).jpg"))
        );
        assert_eq!(
            covers.find(r"D:\MUSIC\Kittie - Oracle (2001)\CD1"),
            Some(PathBuf::from("Kittie - Oracle (2001).jpg"))
        );
        assert_eq!(covers.find(r"D:\MUSIC\Kittie - Spit (1999)"), None);
    }

    #[test]
    fn picks_the_smallest_thumbnail_that_fits() {
        assert_eq!(thumbnail_size(50.), 128);
        assert_eq!(thumbnail_size(128.), 128);
        assert_eq!(thumbnail_size(270.), 512);
        assert_eq!(thumbnail_size(1000.), 1024);
        assert_eq!(thumbnail_size(1500.), 1024);
    }

    #[test]
    fn cache_names_are_stable() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
