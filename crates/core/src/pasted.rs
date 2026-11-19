//! A picture pasted into a terminal. A terminal passes only text on, so the
//! picture is saved as a file and its path is typed instead: Claude Code and
//! other agents take a pasted path to an image as the image itself, and a
//! shell gets a file it can copy or open.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a pasted picture is kept. The program it was pasted into may
/// read it only when it is asked about it, so it must outlive the paste.
const KEEP: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Where pasted pictures go: a folder of Wercode's in the system's
/// temporary folder.
pub fn folder() -> PathBuf {
    std::env::temp_dir().join("wercode-pasted")
}

/// Saves the picture in `dir` and returns its path. `extension` names the
/// kind of picture the bytes hold (`png`, `jpg`, `bmp`, ...). PNG, JPEG, GIF
/// and WebP are kept as they are; anything else the image library reads
/// (a bitmap, which is what a screenshot often is on Windows) becomes PNG,
/// the kind every program that takes pictures reads. Pictures older than a
/// week are removed from `dir` on the way.
pub fn save(bytes: &[u8], extension: &str, dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    forget_old(dir);
    let extension = extension.to_ascii_lowercase();
    let (bytes, extension) = match extension.as_str() {
        "png" | "gif" | "webp" => (bytes.to_vec(), extension.as_str()),
        "jpg" | "jpeg" => (bytes.to_vec(), "jpg"),
        _ => (to_png(bytes)?, "png"),
    };
    write_new(dir, extension, &bytes)
}

fn to_png(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    let picture = image::load_from_memory(bytes).map_err(std::io::Error::other)?;
    let mut png = Vec::new();
    picture
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(std::io::Error::other)?;
    Ok(png)
}

/// Writes `bytes` to a new file `image-<milliseconds>.<extension>`, with a
/// number added if two pictures are pasted within the same millisecond. The
/// file is created only if it does not exist yet, so two pastes at once (two
/// windows, two terminals) cannot both take the same name.
fn write_new(dir: &Path, extension: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    use std::io::Write as _;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis());
    for number in 0..1000 {
        let path = match number {
            0 => dir.join(format!("image-{stamp}.{extension}")),
            n => dir.join(format!("image-{stamp}-{n}.{extension}")),
        };
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(bytes)?;
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::other("no free name for the picture"))
}

fn forget_old(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|modified| now.duration_since(modified).is_ok_and(|age| age > KEEP));
        let ours = entry.file_name().to_string_lossy().starts_with("image-");
        if old && ours {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wercode-pasted-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// A 2×1 picture, red then blue, as the bytes of the given kind.
    fn picture(kind: image::ImageFormat) -> Vec<u8> {
        let mut pixels = image::RgbImage::new(2, 1);
        pixels.put_pixel(0, 0, image::Rgb([255, 0, 0]));
        pixels.put_pixel(1, 0, image::Rgb([0, 0, 255]));
        let mut bytes = Vec::new();
        pixels.write_to(&mut std::io::Cursor::new(&mut bytes), kind).unwrap();
        bytes
    }

    #[test]
    fn keeps_png_and_turns_a_bitmap_into_png() {
        let dir = temp_dir("kinds");
        let png = picture(image::ImageFormat::Png);
        let kept = save(&png, "png", &dir).unwrap();
        assert_eq!(kept.extension().unwrap(), "png");
        assert_eq!(std::fs::read(&kept).unwrap(), png);

        let bitmap = save(&picture(image::ImageFormat::Bmp), "bmp", &dir).unwrap();
        assert_eq!(bitmap.extension().unwrap(), "png");
        let read = image::open(&bitmap).unwrap().to_rgb8();
        assert_eq!((read.width(), read.height()), (2, 1));
        assert_eq!(read.get_pixel(1, 0), &image::Rgb([0, 0, 255]));

        // Two pastes in a row get two files.
        assert_ne!(save(&png, "png", &dir).unwrap(), save(&png, "png", &dir).unwrap());
        // Bytes that are not a picture are an error, not an empty file.
        assert!(save(b"not a picture", "bmp", &dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn removes_only_old_pictures_of_its_own() {
        let dir = temp_dir("old");
        std::fs::create_dir_all(&dir).unwrap();
        let week_ago = SystemTime::now() - KEEP - Duration::from_secs(60);
        for name in ["image-1.png", "notes.txt"] {
            let file = std::fs::File::create(dir.join(name)).unwrap();
            file.set_modified(week_ago).unwrap();
        }
        let fresh = save(&picture(image::ImageFormat::Png), "png", &dir).unwrap();
        assert!(!dir.join("image-1.png").exists());
        assert!(dir.join("notes.txt").exists());
        assert!(fresh.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
