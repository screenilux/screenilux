//! The `.screenilux` file: a whole multi-screen recording in one file.
//!
//! A session folder is awkward to share: it has to be zipped by hand, and a
//! recipient can open the wrong file inside it. A package is that same folder
//! as a ZIP archive, so it is one file to send and can still be inspected with
//! any archive tool by renaming it.
//!
//! Entries are *stored*, not compressed. The videos are already H.264 and
//! would not shrink, and stored entries keep each video a contiguous byte
//! range, which leaves the door open to playing straight out of the archive.
//! ZIP64 is always on, because an hour of 1440p crosses 4GB.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::session::{MANIFEST_NAME, Manifest, ManifestError};
use crate::text::{TEXT_INDEX_NAME, TextIndex, TextIndexError};

pub const PACKAGE_EXT: &str = "screenilux";

#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a valid package: {0}")]
    Archive(#[from] zip::result::ZipError),
    #[error("the package has no {MANIFEST_NAME}")]
    NoManifest,
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    Text(#[from] TextIndexError),
    #[error("the manifest names {0:?}, which is not a plain file name")]
    UnsafeName(String),
    #[error("the package is missing {0}")]
    Missing(String),
}

fn options() -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .large_file(true)
}

/// A file name from a manifest is joined onto a directory, so it must not be
/// able to point anywhere else. A crafted package naming `..\..\x.exe` would
/// otherwise write outside the folder it is unpacked into.
fn check_name(name: &str) -> Result<(), PackageError> {
    let plain = !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && !name.starts_with('.');
    if plain {
        Ok(())
    } else {
        Err(PackageError::UnsafeName(name.to_string()))
    }
}

/// Write `dest` via a temporary sibling, so a failure halfway never leaves a
/// truncated package where a good one, or nothing, was expected.
fn write_atomically(
    dest: &Path,
    fill: impl FnOnce(&mut ZipWriter<BufWriter<File>>) -> Result<(), PackageError>,
) -> Result<(), PackageError> {
    let part = dest.with_extension(format!("{PACKAGE_EXT}.part"));
    let result = (|| {
        let mut zip = ZipWriter::new(BufWriter::new(File::create(&part)?));
        fill(&mut zip)?;
        zip.finish()?.flush()?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            std::fs::rename(&part, dest)?;
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// Pack a session folder into `dest`.
///
/// Only what the manifest names goes in, plus a text index if there is one,
/// so stray files in the folder are not swept along.
pub fn pack(folder: &Path, dest: &Path) -> Result<Manifest, PackageError> {
    let json = std::fs::read_to_string(folder.join(MANIFEST_NAME))?;
    let manifest = Manifest::from_json(&json)?;
    for file in manifest.videos() {
        check_name(file)?;
    }
    let text = std::fs::read(folder.join(TEXT_INDEX_NAME)).ok();

    write_atomically(dest, |zip| {
        // The manifest goes first, so a reader learns what is inside without
        // seeking past gigabytes of video.
        zip.start_file(MANIFEST_NAME, options())?;
        zip.write_all(json.as_bytes())?;
        for file in manifest.videos() {
            zip.start_file(file, options())?;
            let mut src = File::open(folder.join(file))?;
            std::io::copy(&mut src, zip)?;
        }
        if let Some(text) = &text {
            zip.start_file(TEXT_INDEX_NAME, options())?;
            zip.write_all(text)?;
        }
        Ok(())
    })?;
    Ok(manifest)
}

fn open(path: &Path) -> Result<ZipArchive<BufReader<File>>, PackageError> {
    Ok(ZipArchive::new(BufReader::new(File::open(path)?))?)
}

fn read_entry<R: Read + Seek>(
    zip: &mut ZipArchive<R>,
    name: &str,
) -> Result<Option<String>, PackageError> {
    match zip.by_name(name) {
        Ok(mut f) => {
            let mut s = String::new();
            f.read_to_string(&mut s)?;
            Ok(Some(s))
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn manifest_of<R: Read + Seek>(zip: &mut ZipArchive<R>) -> Result<Manifest, PackageError> {
    let json = read_entry(zip, MANIFEST_NAME)?.ok_or(PackageError::NoManifest)?;
    let manifest = Manifest::from_json(&json)?;
    for file in manifest.videos() {
        check_name(file)?;
    }
    Ok(manifest)
}

/// The manifest alone, without touching the videos.
pub fn read_manifest(path: &Path) -> Result<Manifest, PackageError> {
    manifest_of(&mut open(path)?)
}

/// The text index, if the package has one.
pub fn read_text_index(path: &Path) -> Result<Option<TextIndex>, PackageError> {
    match read_entry(&mut open(path)?, TEXT_INDEX_NAME)? {
        Some(json) => Ok(Some(TextIndex::from_json(&json)?)),
        None => Ok(None),
    }
}

/// Unpack into `into`, which becomes an ordinary session folder. Returns the
/// manifest. Anything in the archive the manifest does not name is ignored.
pub fn unpack(path: &Path, into: &Path) -> Result<Manifest, PackageError> {
    let mut zip = open(path)?;
    let manifest = manifest_of(&mut zip)?;
    std::fs::create_dir_all(into)?;
    std::fs::write(into.join(MANIFEST_NAME), manifest.to_json()?)?;

    let mut names: Vec<String> = manifest.videos().map(str::to_string).collect();
    names.push(TEXT_INDEX_NAME.to_string());
    for name in names {
        let mut entry = match zip.by_name(&name) {
            Ok(e) => e,
            Err(zip::result::ZipError::FileNotFound) if name == TEXT_INDEX_NAME => continue,
            Err(zip::result::ZipError::FileNotFound) => return Err(PackageError::Missing(name)),
            Err(e) => return Err(e.into()),
        };
        let mut out = BufWriter::new(File::create(into.join(&name))?);
        std::io::copy(&mut entry, &mut out)?;
        out.flush()?;
    }
    Ok(manifest)
}

/// Add or replace the text index inside a package.
///
/// Appending is cheap. Replacing an index already there means rewriting the
/// archive, since ZIP cannot drop an entry in place, but the videos are copied
/// raw rather than re-read entry by entry.
pub fn write_text_index(path: &Path, index: &TextIndex) -> Result<(), PackageError> {
    let json = index.to_json()?;
    let has_index = open(path)?.index_for_name(TEXT_INDEX_NAME).is_some();

    if !has_index {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        let mut zip = ZipWriter::new_append(file)?;
        zip.start_file(TEXT_INDEX_NAME, options())?;
        zip.write_all(json.as_bytes())?;
        zip.finish()?;
        return Ok(());
    }

    let mut src = open(path)?;
    write_atomically(path, |zip| {
        for i in 0..src.len() {
            let entry = src.by_index_raw(i)?;
            if entry.name()? == TEXT_INDEX_NAME {
                continue;
            }
            zip.raw_copy_file(entry)?;
        }
        zip.start_file(TEXT_INDEX_NAME, options())?;
        zip.write_all(json.as_bytes())?;
        Ok(())
    })
}

/// Whether a path looks like a package, by extension.
pub fn is_package(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(PACKAGE_EXT))
}

/// `dir/stem.screenilux`.
pub fn package_path(dir: &Path, stem: &str) -> PathBuf {
    dir.join(format!("{stem}.{PACKAGE_EXT}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Role, Screen};
    use crate::text::{Line, Page, Word};

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "screenilux-pkg-{}-{name}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn session(dir: &Path) -> Manifest {
        let mut m = Manifest::new("2026-10-31T12:00:00".into(), 3.0, 30, "x".into(), false);
        for i in 1..=2 {
            let file = format!("screen{i}.mp4");
            std::fs::write(dir.join(&file), vec![i as u8; 4096 * i]).unwrap();
            m.screens.push(Screen {
                file,
                label: format!("Screen {i}"),
                width: 1920,
                height: 1080,
                left: 0,
                top: 0,
                primary: i == 1,
                audio: i == 1,
                role: if i == 1 {
                    Role::Primary
                } else {
                    Role::Secondary
                },
                order: 0,
            });
        }
        m.assign_order();
        std::fs::write(dir.join(MANIFEST_NAME), m.to_json().unwrap()).unwrap();
        m
    }

    fn index(word: &str) -> TextIndex {
        let mut idx = TextIndex::new(2.0);
        idx.record(
            "screen1.mp4",
            1.0,
            Page {
                lines: vec![Line {
                    words: vec![Word {
                        text: word.into(),
                        x: 0.0,
                        y: 0.0,
                        w: 1.0,
                        h: 1.0,
                    }],
                }],
            },
        );
        idx
    }

    #[test]
    fn a_session_round_trips_through_a_package() {
        let s = Scratch::new("roundtrip");
        let folder = s.0.join("rec");
        std::fs::create_dir_all(&folder).unwrap();
        let m = session(&folder);
        std::fs::write(folder.join("stray.txt"), "not named").unwrap();

        let pkg = package_path(&s.0, "rec");
        pack(&folder, &pkg).unwrap();
        assert!(is_package(&pkg));
        assert_eq!(read_manifest(&pkg).unwrap(), m);

        let out = s.0.join("out");
        let back = unpack(&pkg, &out).unwrap();
        assert_eq!(back, m);
        for screen in &m.screens {
            assert_eq!(
                std::fs::read(out.join(&screen.file)).unwrap(),
                std::fs::read(folder.join(&screen.file)).unwrap()
            );
        }
        assert!(
            !out.join("stray.txt").exists(),
            "only named files are packed"
        );
        assert!(!pkg.with_extension("screenilux.part").exists());
    }

    #[test]
    fn videos_are_stored_uncompressed() {
        let s = Scratch::new("stored");
        let folder = s.0.join("rec");
        std::fs::create_dir_all(&folder).unwrap();
        session(&folder);
        let pkg = package_path(&s.0, "rec");
        pack(&folder, &pkg).unwrap();
        let mut zip = open(&pkg).unwrap();
        let entry = zip.by_name("screen2.mp4").unwrap();
        assert_eq!(entry.compression(), CompressionMethod::Stored);
        assert_eq!(entry.compressed_size(), entry.size());
    }

    #[test]
    fn a_text_index_can_be_added_then_replaced() {
        let s = Scratch::new("text");
        let folder = s.0.join("rec");
        std::fs::create_dir_all(&folder).unwrap();
        session(&folder);
        let pkg = package_path(&s.0, "rec");
        pack(&folder, &pkg).unwrap();
        assert_eq!(read_text_index(&pkg).unwrap(), None);

        write_text_index(&pkg, &index("first")).unwrap();
        assert_eq!(read_text_index(&pkg).unwrap(), Some(index("first")));

        write_text_index(&pkg, &index("second")).unwrap();
        assert_eq!(read_text_index(&pkg).unwrap(), Some(index("second")));
        // Replacing must not duplicate or lose anything else.
        let zip = open(&pkg).unwrap();
        assert_eq!(zip.len(), 4, "manifest, two screens, one index");

        let out = s.0.join("out");
        unpack(&pkg, &out).unwrap();
        assert!(out.join(TEXT_INDEX_NAME).is_file());
    }

    #[test]
    fn a_missing_video_is_reported() {
        let s = Scratch::new("missing");
        let folder = s.0.join("rec");
        std::fs::create_dir_all(&folder).unwrap();
        let m = session(&folder);
        // Build an archive whose manifest names a file it does not contain.
        let pkg = package_path(&s.0, "broken");
        write_atomically(&pkg, |zip| {
            zip.start_file(MANIFEST_NAME, options())?;
            zip.write_all(m.to_json().unwrap().as_bytes())?;
            Ok(())
        })
        .unwrap();
        assert!(matches!(
            unpack(&pkg, &s.0.join("out")),
            Err(PackageError::Missing(_))
        ));
    }

    #[test]
    fn a_manifest_naming_a_path_outside_the_folder_is_refused() {
        for bad in [
            "../evil.mp4",
            "..\\evil.mp4",
            "C:evil.mp4",
            "sub/x.mp4",
            "",
            "..",
        ] {
            assert!(check_name(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(check_name("screen1.mp4").is_ok());
    }

    #[test]
    fn something_that_is_not_a_zip_is_an_error_not_a_panic() {
        let s = Scratch::new("junk");
        let p = s.0.join("junk.screenilux");
        std::fs::write(&p, b"definitely not a zip").unwrap();
        assert!(read_manifest(&p).is_err());
    }
}
