//! Game-root detection and resource opening for AVG32 installations.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::ini::Ini;
use crate::pdt::{PdtImage, decode_pdt};
use crate::resource::{Avg32Resources, find_case_insensitive};
use crate::scene::Avg32SceneHeader;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    Avg32,
    Siglus,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameLayout {
    pub root: PathBuf,
    pub kind: EngineKind,
    pub gameexe_ini: Option<PathBuf>,
    pub seen_archive: Option<PathBuf>,
    pub pdt_root: Option<PathBuf>,
}

/// Distinguishes AVG32 from Siglus by on-disk formats, not publisher or title.
pub fn detect_game_root(root: impl AsRef<Path>) -> Result<GameLayout> {
    let root = root
        .as_ref()
        .canonicalize()
        .with_context(|| format!("invalid game root {}", root.as_ref().display()))?;
    if !root.is_dir() {
        bail!("{} is not a game directory", root.display());
    }
    let gameexe_ini = find_case_insensitive(&root, Path::new("Gameexe.ini"));
    if find_case_insensitive(&root, Path::new("Scene.pck")).is_some()
        || find_case_insensitive(&root, Path::new("Gameexe.dat")).is_some()
    {
        return Ok(GameLayout {
            root,
            kind: EngineKind::Siglus,
            gameexe_ini,
            seen_archive: None,
            pdt_root: None,
        });
    }
    let seen_archive = [Path::new("DAT/SEEN.TXT"), Path::new("SEEN.TXT")]
        .into_iter()
        .find_map(|path| find_case_insensitive(&root, path));
    let pdt_root = [Path::new("PDT"), Path::new("DAT/PDT")]
        .into_iter()
        .find_map(|path| find_case_insensitive(&root, path))
        .filter(|path| path.is_dir());
    let is_avg32_seen = seen_archive
        .as_deref()
        .and_then(|path| std::fs::read(path).ok())
        .is_some_and(|bytes| bytes.starts_with(b"PACL"));
    let has_loose_scenes = [Path::new("DAT"), Path::new("")]
        .into_iter()
        .filter_map(|directory| {
            find_case_insensitive(&root, directory).or_else(|| Some(root.clone()))
        })
        .filter_map(|directory| std::fs::read_dir(directory).ok())
        .flatten()
        .flatten()
        .any(|entry| {
            entry.file_name().to_str().is_some_and(|name| {
                let upper = name.to_ascii_uppercase();
                upper.starts_with("SEEN") && upper.ends_with(".TXT") && upper.len() > 8
            })
        });
    let kind = if gameexe_ini.is_some() && (is_avg32_seen || has_loose_scenes) {
        EngineKind::Avg32
    } else {
        EngineKind::Unknown
    };
    Ok(GameLayout {
        root,
        kind,
        gameexe_ini,
        seen_archive,
        pdt_root,
    })
}

#[derive(Debug)]
pub struct Avg32Game {
    pub layout: GameLayout,
    pub ini: Ini,
    pub resources: Avg32Resources,
}

/// Decodes a text file: UTF-8 when valid, then the NLS encoding, then
/// Shift-JIS.
pub fn decode_text(bytes: &[u8]) -> String {
    crate::nls::decode_file_text(bytes)
}

impl Avg32Game {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let layout = detect_game_root(root)?;
        if layout.kind != EngineKind::Avg32 {
            bail!(
                "{} is not an AVG32 game root ({:?})",
                layout.root.display(),
                layout.kind
            );
        }
        let gameexe_path = layout
            .gameexe_ini
            .as_ref()
            .expect("AVG32 layout has Gameexe.ini");
        let gameexe = decode_text(
            &std::fs::read(gameexe_path)
                .with_context(|| format!("failed to read {}", gameexe_path.display()))?,
        );
        let setup = find_case_insensitive(&layout.root, Path::new("SETUP.INI"))
            .and_then(|path| std::fs::read(path).ok())
            .map(|bytes| decode_text(&bytes));
        let ini = Ini::parse(&gameexe, setup.as_deref());
        let resources = Avg32Resources::from_ini(&layout.root, &ini);
        Ok(Self {
            layout,
            ini,
            resources,
        })
    }

    /// Scene names in the scenario archive (or loose `SEEN###.TXT` files).
    pub fn scene_names(&self) -> Vec<String> {
        let Some(route) = self.resources.route("TXT") else {
            return Vec::new();
        };
        if let Some(archive) = &route.archive {
            if let Some(path) = find_case_insensitive(&route.directory, archive) {
                if let Ok(archive) = crate::archive::PaclArchive::from_path(path) {
                    return archive
                        .entries()
                        .iter()
                        .map(|entry| entry.name.clone())
                        .collect();
                }
            }
        }
        let mut names: Vec<String> = std::fs::read_dir(&route.directory)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
            .filter(|name| {
                let upper = name.to_ascii_uppercase();
                upper.starts_with("SEEN") && upper.ends_with(".TXT") && upper.len() > 8
            })
            .collect();
        names.sort();
        names
    }

    pub fn read_scene(&self, name: &str) -> Result<Vec<u8>> {
        self.resources.read("TXT", name)
    }

    /// `SEEN%03d.TXT` for scene number `seen`.
    pub fn read_seen(&self, seen: i32) -> Result<Vec<u8>> {
        self.read_scene(&format!("SEEN{seen:03}.TXT"))
    }

    pub fn scene_header(&self, name: &str) -> Result<Avg32SceneHeader> {
        Avg32SceneHeader::parse(&self.read_scene(name)?)
            .with_context(|| format!("failed to parse AVG32 scene {name}"))
    }

    /// The `#SEEN_START` scene name.
    pub fn configured_start_scene(&self) -> Option<String> {
        let name = format!("SEEN{:03}.TXT", self.ini.start_seen);
        self.resources.exists("TXT", &name).then_some(name)
    }

    pub fn load_pdt(&self, name: &str) -> Result<PdtImage> {
        decode_pdt(&self.resources.read("PDT", name)?)
            .with_context(|| format!("failed to decode PDT {name}"))
    }
}
