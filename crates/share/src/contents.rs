//! What gets uploaded for a recording.
//!
//! A package already holds the manifest, the videos and maybe a text index;
//! each entry is uploaded as its own file. Packages store their entries
//! uncompressed, so each is read as a plain range of the archive's bytes.
//! A single-screen MP4 has no manifest, so one is made for it, and the video
//! goes up as `screen1.mp4`. The web player then only ever sees one shape.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use screenilux_core::session::{MANIFEST_NAME, Manifest, Role, Screen};
use screenilux_core::text::TEXT_INDEX_NAME;
use zip::{CompressionMethod, ZipArchive};

use crate::ShareError;

/// One file of a shared recording.
#[derive(Debug, Clone)]
pub struct Item {
    pub name: String,
    pub len: u64,
    source: Source,
}

#[derive(Debug, Clone)]
enum Source {
    File(PathBuf),
    /// `len` bytes of a file, from `start`.
    Range(PathBuf, u64),
    Bytes(Vec<u8>),
}

impl Item {
    pub(crate) fn open(&self) -> Result<Box<dyn Read + '_>, ShareError> {
        let read = |e: std::io::Error| ShareError::Read(e.to_string());
        Ok(match &self.source {
            Source::File(path) => Box::new(BufReader::new(File::open(path).map_err(read)?)),
            Source::Range(path, start) => {
                let mut file = File::open(path).map_err(read)?;
                file.seek(SeekFrom::Start(*start)).map_err(read)?;
                Box::new(BufReader::new(file).take(self.len))
            }
            Source::Bytes(bytes) => Box::new(&bytes[..]),
        })
    }
}

/// A recording, ready to upload.
#[derive(Debug, Clone)]
pub struct Contents {
    pub title: String,
    /// When it was recorded, RFC 3339.
    pub created: String,
    pub items: Vec<Item>,
}

impl Contents {
    pub fn total(&self) -> u64 {
        self.items.iter().map(|i| i.len).sum()
    }

    /// A `.screenilux` package.
    pub fn of_package(path: &Path, title: String) -> Result<Self, ShareError> {
        let read = |e: &dyn std::fmt::Display| ShareError::Read(e.to_string());
        let manifest = screenilux_core::package::read_manifest(path).map_err(|e| read(&e))?;
        let mut zip = ZipArchive::new(BufReader::new(File::open(path).map_err(|e| read(&e))?))
            .map_err(|e| read(&e))?;

        let mut names = vec![MANIFEST_NAME.to_string()];
        names.extend(manifest.screens.iter().map(|s| s.file.clone()));
        if zip.index_for_name(TEXT_INDEX_NAME).is_some() {
            names.push(TEXT_INDEX_NAME.to_string());
        }
        let mut items = Vec::new();
        for name in names {
            let entry = zip.by_name(&name).map_err(|e| read(&e))?;
            let start = match (entry.compression(), entry.data_start()) {
                (CompressionMethod::Stored, Some(start)) => start,
                _ => {
                    return Err(ShareError::Read(format!(
                        "{name} is compressed in the package"
                    )));
                }
            };
            items.push(Item {
                len: entry.size(),
                source: Source::Range(path.to_path_buf(), start),
                name,
            });
        }
        Ok(Self {
            title,
            created: manifest.created,
            items,
        })
    }

    /// A single-screen recording saved as a plain MP4. It has no record of
    /// when it was made other than the file's own time.
    pub fn of_video(
        path: &Path,
        title: String,
        (width, height): (u32, u32),
        duration: f64,
        audio: bool,
    ) -> Result<Self, ShareError> {
        let meta = std::fs::metadata(path).map_err(|e| ShareError::Read(e.to_string()))?;
        let len = meta.len();
        let created = humantime::format_rfc3339_seconds(
            meta.modified()
                .unwrap_or_else(|_| std::time::SystemTime::now()),
        )
        .to_string();
        let file = "screen1.mp4".to_string();
        let mut manifest = Manifest::new(created.clone(), duration, 30, String::new(), false);
        manifest.screens.push(Screen {
            file: file.clone(),
            label: "Screen".into(),
            width,
            height,
            left: 0,
            top: 0,
            primary: true,
            audio,
            role: Role::Primary,
            order: 1,
        });
        let json = manifest
            .to_json()
            .map_err(|e| ShareError::Read(e.to_string()))?
            .into_bytes();
        Ok(Self {
            title,
            created,
            items: vec![
                Item {
                    name: MANIFEST_NAME.into(),
                    len: json.len() as u64,
                    source: Source::Bytes(json),
                },
                Item {
                    name: file,
                    len,
                    source: Source::File(path.to_path_buf()),
                },
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all(item: &Item) -> Vec<u8> {
        let mut out = Vec::new();
        item.open().unwrap().read_to_end(&mut out).unwrap();
        out
    }

    #[test]
    fn a_package_is_uploaded_entry_by_entry() {
        let dir =
            std::env::temp_dir().join(format!("screenilux-share-test-{}", std::process::id()));
        let folder = dir.join("session");
        std::fs::create_dir_all(&folder).unwrap();
        let mut manifest =
            Manifest::new("2026-11-01T10:00:00Z".into(), 3.0, 30, "test".into(), false);
        for (i, primary) in [(1, true), (2, false)] {
            manifest.screens.push(Screen {
                file: format!("screen{i}.mp4"),
                label: format!("Screen {i}"),
                width: 1920,
                height: 1080,
                left: 0,
                top: 0,
                primary,
                audio: primary,
                role: if primary {
                    Role::Primary
                } else {
                    Role::Secondary
                },
                order: i,
            });
            std::fs::write(
                folder.join(format!("screen{i}.mp4")),
                vec![i as u8; 1000 * i as usize],
            )
            .unwrap();
        }
        std::fs::write(folder.join(MANIFEST_NAME), manifest.to_json().unwrap()).unwrap();
        let package = dir.join("r.screenilux");
        screenilux_core::package::pack(&folder, &package).unwrap();

        let contents = Contents::of_package(&package, "R".into()).unwrap();
        let names: Vec<_> = contents.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["recording.json", "screen1.mp4", "screen2.mp4"]);
        assert_eq!(contents.total(), contents.items[0].len + 3000);
        assert_eq!(read_all(&contents.items[1]), vec![1u8; 1000]);
        assert_eq!(read_all(&contents.items[2]), vec![2u8; 2000]);
        let manifest_back: Manifest =
            serde_json::from_slice(&read_all(&contents.items[0])).unwrap();
        assert_eq!(manifest_back.screens.len(), 2);
        assert_eq!(contents.created, "2026-11-01T10:00:00Z");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_video_gets_a_manifest_of_its_own() {
        let dir =
            std::env::temp_dir().join(format!("screenilux-share-video-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("r.mp4");
        std::fs::write(&video, [0u8; 500]).unwrap();

        let contents = Contents::of_video(&video, "R".into(), (2560, 1440), 9.5, true).unwrap();
        let manifest: Manifest = serde_json::from_slice(&read_all(&contents.items[0])).unwrap();
        assert_eq!(manifest.screens[0].file, "screen1.mp4");
        assert_eq!((manifest.screens[0].width, manifest.duration), (2560, 9.5));
        assert_eq!(contents.items[1].len, 500);
        assert!(contents.created.ends_with('Z'));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
