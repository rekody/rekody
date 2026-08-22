//! Cut archives: the single file a cut travels as.
//!
//! A cut is a folder (manifest.jsonl, cut.json, and `audio/` when it is
//! self-contained). To move one between machines it has to become one
//! file, so this module zips a cut folder for the browser download and the
//! `--zip` export, and opens an incoming cut, folder or zip alike, for the
//! import.
//!
//! Reading a zip is the untrusted direction: the file arrived by mail, or
//! from a drive, or from a browser upload. Every entry has to earn its
//! place before it is written, so extraction takes only the three shapes a
//! cut can hold (`manifest.jsonl`, `cut.json`, `audio/<name>`), refuses any
//! path that could resolve outside the destination, and stops at a total
//! size cap rather than unpacking whatever it is handed.

use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Ceiling on what one import may unpack. A personal cut is tens of
/// megabytes; this is generous for a real one and finite for a hostile one.
pub const MAX_UNPACKED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Write `dir` into `out` as a zip, every entry under the folder's own
/// name, so unzipping in Downloads produces one tidy `cut-…` folder rather
/// than loose files.
pub fn write_zip<W: Write + Seek>(dir: &Path, out: W) -> Result<()> {
    let root = dir
        .file_name()
        .and_then(|n| n.to_str())
        .context("the cut folder has no name")?
        .to_string();
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut zw = zip::ZipWriter::new(out);

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for name in ["manifest.jsonl", "cut.json"] {
        let path = dir.join(name);
        if path.is_file() {
            files.push((format!("{root}/{name}"), path));
        }
    }
    let audio = dir.join("audio");
    if audio.is_dir() {
        let mut clips: Vec<PathBuf> = std::fs::read_dir(&audio)
            .with_context(|| format!("reading {}", audio.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .collect();
        clips.sort();
        for clip in clips {
            let Some(name) = clip.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            files.push((format!("{root}/audio/{name}"), clip.clone()));
        }
    }

    for (entry, path) in files {
        zw.start_file(&entry, opts)
            .with_context(|| format!("starting zip entry {entry}"))?;
        let mut f =
            std::fs::File::open(&path).with_context(|| format!("reading {}", path.display()))?;
        std::io::copy(&mut f, &mut zw).with_context(|| format!("writing zip entry {entry}"))?;
    }
    zw.finish().context("finishing the zip")?;
    Ok(())
}

/// An opened cut: a folder on disk holding a manifest, plus the temp dir
/// keeping it alive when the cut arrived as a zip.
pub struct OpenCut {
    /// The folder holding manifest.jsonl.
    pub root: PathBuf,
    /// `cut-<until>-<hash8>` when cut.json says so, else the file name the
    /// cut arrived under. Recorded as `merged_from` on merged decisions.
    pub label: String,
    _unpacked: Option<tempfile::TempDir>,
}

/// Open a cut for import: a folder as-is, or a zip unpacked into a temp dir
/// that lives as long as the returned handle.
pub fn open(path: &Path) -> Result<OpenCut> {
    anyhow::ensure!(path.exists(), "no cut at {}", path.display());
    let fallback = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("imported cut")
        .to_string();

    if path.is_dir() {
        let root = find_cut_root(path)
            .with_context(|| format!("no manifest.jsonl inside {}", path.display()))?;
        let label = label_for(&root, &fallback);
        return Ok(OpenCut {
            root,
            label,
            _unpacked: None,
        });
    }

    let tmp = tempfile::tempdir().context("making a folder to unpack the cut into")?;
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    unpack(file, tmp.path())?;
    let root = find_cut_root(tmp.path())
        .with_context(|| format!("{} has no manifest.jsonl in it", path.display()))?;
    let label = label_for(&root, &fallback);
    Ok(OpenCut {
        root,
        label,
        _unpacked: Some(tmp),
    })
}

/// Unpack a zip into `dest`, taking only the entries a cut is made of.
pub fn unpack<R: Read + Seek>(source: R, dest: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(source).context("reading the zip")?;
    let mut budget = MAX_UNPACKED_BYTES;
    let mut took = 0usize;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).context("reading a zip entry")?;
        if entry.is_dir() {
            continue;
        }
        // enclosed_name() is the zip-slip guard: it refuses absolute paths,
        // parent-directory hops, and anything else that would resolve
        // outside dest. The shape check then keeps what a cut can hold.
        let Some(name) = entry.enclosed_name().and_then(|n| cut_entry(&n)) else {
            tracing::warn!(entry = entry.name(), "not part of a cut; skipping");
            continue;
        };
        let out = dest.join(&name);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut file =
            std::fs::File::create(&out).with_context(|| format!("writing {}", out.display()))?;
        // Copy against the remaining budget rather than the entry's own
        // claim about its size, which a crafted zip is free to lie about.
        let written = std::io::copy(&mut entry.by_ref().take(budget), &mut file)
            .with_context(|| format!("unpacking {}", name.display()))?;
        budget = budget.saturating_sub(written);
        took += 1;
        anyhow::ensure!(
            budget > 0,
            "this cut unpacks to more than {} GB; refusing it",
            MAX_UNPACKED_BYTES / (1024 * 1024 * 1024)
        );
    }
    anyhow::ensure!(took > 0, "that zip holds nothing a cut is made of");
    Ok(())
}

/// The path a zip entry may be unpacked to, or `None` when it is not one of
/// the three shapes a cut holds. A single wrapping folder is allowed, since
/// that is how both this exporter and Finder zip a cut.
fn cut_entry(name: &Path) -> Option<PathBuf> {
    let parts: Vec<&str> = name
        .components()
        .map(|c| c.as_os_str().to_str().unwrap_or_default())
        .collect();
    if parts
        .iter()
        .any(|p| p.is_empty() || *p == "." || *p == "..")
    {
        return None;
    }
    let tail: Vec<&str> = match parts.as_slice() {
        [file] => vec![file],
        [dir, file] if *dir == "audio" => vec![dir, file],
        [_wrap, file] => vec![file],
        [_wrap, dir, file] if *dir == "audio" => vec![dir, file],
        _ => return None,
    };
    match tail.as_slice() {
        ["manifest.jsonl"] | ["cut.json"] => Some(PathBuf::from(tail[0])),
        ["audio", file] => crate::store::cut_audio_name(file).map(|f| Path::new("audio").join(f)),
        _ => None,
    }
}

/// The folder holding manifest.jsonl: `dir` itself, or the one subfolder
/// that has it (an unpacked zip that carried its `cut-…` wrapper).
fn find_cut_root(dir: &Path) -> Option<PathBuf> {
    if dir.join("manifest.jsonl").is_file() {
        return Some(dir.to_path_buf());
    }
    let mut found = None;
    for entry in std::fs::read_dir(dir).ok()? {
        let path = entry.ok()?.path();
        if path.is_dir() && path.join("manifest.jsonl").is_file() {
            if found.is_some() {
                return None; // more than one cut in there; too ambiguous
            }
            found = Some(path);
        }
    }
    found
}

/// `cut-<until>-<hash8>` from the cut's own cut.json, so provenance names
/// the cut itself and not whatever the file was renamed to in transit.
fn label_for(root: &Path, fallback: &str) -> String {
    let raw = std::fs::read_to_string(root.join("cut.json")).unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
    match (v["until"].as_str(), v["manifest_sha256"].as_str()) {
        (Some(until), Some(sha)) if sha.len() >= 8 => format!("cut-{until}-{}", &sha[..8]),
        _ => fallback.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_cut(dir: &Path) {
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        std::fs::write(
            dir.join("manifest.jsonl"),
            "{\"audio_filepath\":\"audio/a.wav\",\"text\":\"alpha\"}\n",
        )
        .unwrap();
        std::fs::write(dir.join("cut.json"), "{\"until\":\"2026-08-19\"}").unwrap();
        std::fs::write(dir.join("audio").join("a.wav"), b"RIFFfake").unwrap();
    }

    #[test]
    fn zip_round_trip_keeps_the_cut_intact() {
        let tmp = tempfile::tempdir().unwrap();
        let cut = tmp.path().join("cut-2026-08-19-deadbeef");
        seed_cut(&cut);

        let zip_path = tmp.path().join("cut.zip");
        write_zip(&cut, std::fs::File::create(&zip_path).unwrap()).unwrap();

        let opened = open(&zip_path).unwrap();
        // The wrapping folder came back, and the root is the cut inside it.
        assert!(opened.root.join("manifest.jsonl").is_file());
        assert_eq!(
            std::fs::read_to_string(opened.root.join("manifest.jsonl")).unwrap(),
            std::fs::read_to_string(cut.join("manifest.jsonl")).unwrap()
        );
        assert_eq!(
            std::fs::read(opened.root.join("audio").join("a.wav")).unwrap(),
            b"RIFFfake"
        );
        assert!(opened.root.join("cut.json").is_file());
    }

    #[test]
    fn a_plain_folder_opens_as_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let cut = tmp.path().join("cut-2026-08-19-deadbeef");
        seed_cut(&cut);
        assert_eq!(open(&cut).unwrap().root, cut);
        // A folder holding the cut works too (an unzipped download).
        assert_eq!(open(tmp.path()).unwrap().root, cut);
    }

    #[test]
    fn the_label_comes_from_cut_json() {
        let tmp = tempfile::tempdir().unwrap();
        let cut = tmp.path().join("whatever-they-renamed-it");
        seed_cut(&cut);
        // No sha in cut.json: fall back to the name it arrived under.
        assert_eq!(open(&cut).unwrap().label, "whatever-they-renamed-it");
        std::fs::write(
            cut.join("cut.json"),
            "{\"until\":\"2026-08-19\",\"manifest_sha256\":\"1a2b3c4d5e6f\"}",
        )
        .unwrap();
        assert_eq!(open(&cut).unwrap().label, "cut-2026-08-19-1a2b3c4d");
    }

    #[test]
    fn entry_guard_takes_only_what_a_cut_holds() {
        let ok = |p: &str| cut_entry(Path::new(p)).is_some();
        assert!(ok("manifest.jsonl"));
        assert!(ok("cut.json"));
        assert!(ok("audio/a.wav"));
        assert!(ok("cut-2026-08-19-deadbeef/manifest.jsonl"));
        assert!(ok("cut-2026-08-19-deadbeef/audio/a.flac"));
        for bad in [
            "audio/../manifest.jsonl",
            "../escape.jsonl",
            "a/b/c/manifest.jsonl",
            "audio/nested/a.wav",
            "audio/a.mp3",
            "audio/.hidden.wav",
            "notes.txt",
            "cut-x/notes.txt",
        ] {
            assert!(!ok(bad), "entry guard let through {bad}");
        }
    }

    #[test]
    fn unpacking_refuses_a_zip_with_nothing_useful_in_it() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("junk.zip");
        {
            let mut zw = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            zw.start_file("notes.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            zw.write_all(b"nothing to see").unwrap();
            zw.finish().unwrap();
        }
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let err = unpack(std::fs::File::open(&path).unwrap(), &dest).unwrap_err();
        assert!(err.to_string().contains("nothing a cut is made of"));
    }
}
