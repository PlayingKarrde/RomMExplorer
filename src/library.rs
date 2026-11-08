//! The local library: where installed roms live on disk and the manifest that
//! tracks them.
//!
//! Layout:  `<library root>/<platform folder>/<rom file or folder>`
//! The manifest (`.romm-explorer.json`) sits in the library root so the
//! library stays portable between machines.

use crate::api::{Progress, Rom, RommClient, CANCELLED, STAGE_EXTRACTING};
use crate::util::{dir_size, now_ts, sanitize_component};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

const MANIFEST: &str = ".romm-explorer.json";

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Debug, Hash)]
pub enum FolderNaming {
    /// RomM's folder name for the platform, e.g. `snes`, `gba`, `psx`.
    #[default]
    FsSlug,
    /// RomM's canonical platform slug.
    Slug,
    /// Human readable name, e.g. `Super Nintendo Entertainment System`.
    DisplayName,
}

impl FolderNaming {
    pub const ALL: [FolderNaming; 3] = [FolderNaming::FsSlug, FolderNaming::Slug, FolderNaming::DisplayName];

    pub fn label(&self) -> &'static str {
        match self {
            FolderNaming::FsSlug => "RomM folder name  (snes, gba, psx…)",
            FolderNaming::Slug => "Platform slug  (snes, gba, ps…)",
            FolderNaming::DisplayName => "Platform name  (Super Nintendo…)",
        }
    }
}

pub fn platform_folder(naming: FolderNaming, fs_slug: &str, slug: &str, display: &str) -> String {
    let order: [&str; 3] = match naming {
        FolderNaming::FsSlug => [fs_slug, slug, display],
        FolderNaming::Slug => [slug, fs_slug, display],
        FolderNaming::DisplayName => [display, fs_slug, slug],
    };
    let raw = order.iter().find(|s| !s.trim().is_empty()).copied().unwrap_or("unknown");
    sanitize_component(raw)
}

pub fn rom_folder(rom: &Rom, naming: FolderNaming) -> String {
    platform_folder(naming, &rom.platform_fs_slug, &rom.platform_slug, rom.platform_label())
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct InstalledRom {
    pub rom_id: i64,
    pub name: String,
    pub platform_id: i64,
    #[serde(default)]
    pub platform_slug: String,
    #[serde(default)]
    pub platform_fs_slug: String,
    #[serde(default)]
    pub platform_name: String,
    /// Path relative to the library root.
    pub rel_path: PathBuf,
    pub size_bytes: u64,
    pub installed_at: i64,
}

#[derive(Default, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    roms: BTreeMap<i64, InstalledRom>,
}

pub struct Library {
    pub root: PathBuf,
    manifest: Manifest,
    /// Bumped on every change so views can cheaply detect updates.
    pub generation: u64,
}

impl Library {
    pub fn open(root: &Path) -> Result<Self> {
        fs::create_dir_all(root).with_context(|| format!("Could not create {}", root.display()))?;
        let path = root.join(MANIFEST);
        let manifest = if path.exists() {
            let text = fs::read_to_string(&path)?;
            serde_json::from_str(&text).context("Library manifest is corrupt")?
        } else {
            Manifest { version: 1, ..Default::default() }
        };
        Ok(Self { root: root.to_path_buf(), manifest, generation: 1 })
    }

    pub fn save(&self) -> Result<()> {
        let path = self.root.join(MANIFEST);
        let tmp = self.root.join(format!("{MANIFEST}.tmp"));
        fs::write(&tmp, serde_json::to_string_pretty(&self.manifest)?)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn changed(&mut self) -> Result<()> {
        self.generation += 1;
        self.save()
    }

    pub fn get(&self, id: i64) -> Option<&InstalledRom> {
        self.manifest.roms.get(&id)
    }

    pub fn is_installed(&self, id: i64) -> bool {
        self.manifest.roms.contains_key(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &InstalledRom> {
        self.manifest.roms.values()
    }

    pub fn count(&self) -> usize {
        self.manifest.roms.len()
    }

    pub fn total_size(&self) -> u64 {
        self.iter().map(|r| r.size_bytes).sum()
    }

    pub fn abs_path(&self, entry: &InstalledRom) -> PathBuf {
        self.root.join(&entry.rel_path)
    }

    pub fn record(&mut self, entry: InstalledRom) -> Result<()> {
        self.manifest.roms.insert(entry.rom_id, entry);
        self.changed()
    }

    fn remove_if_empty(&self, dir: &Path) {
        if dir != self.root && dir.starts_with(&self.root) {
            let empty = fs::read_dir(dir).map(|mut d| d.next().is_none()).unwrap_or(false);
            if empty {
                let _ = fs::remove_dir(dir);
            }
        }
    }

    pub fn uninstall(&mut self, id: i64) -> Result<()> {
        let Some(entry) = self.manifest.roms.get(&id).cloned() else { return Ok(()) };
        let path = self.abs_path(&entry);
        if entry.rel_path.as_os_str().is_empty() || !path.starts_with(&self.root) || path == self.root {
            bail!("Refusing to delete suspicious path {}", path.display());
        }
        let res = if path.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        if let Err(e) = res {
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(e).with_context(|| format!("Could not delete {}", path.display()));
            }
        }
        if let Some(parent) = path.parent() {
            self.remove_if_empty(parent);
        }
        self.manifest.roms.remove(&id);
        self.changed()
    }

    /// Drops manifest entries whose files have vanished. Returns how many.
    pub fn verify(&mut self) -> Result<usize> {
        let missing: Vec<i64> = self
            .manifest
            .roms
            .values()
            .filter(|e| !self.abs_path(e).exists())
            .map(|e| e.rom_id)
            .collect();
        for id in &missing {
            self.manifest.roms.remove(id);
        }
        if !missing.is_empty() {
            self.changed()?;
        }
        Ok(missing.len())
    }

    /// Finds roms already present in the expected location and adds them to
    /// the manifest. Returns how many were adopted.
    pub fn adopt_existing(&mut self, roms: &[Rom], naming: FolderNaming) -> Result<usize> {
        let mut adopted = 0;
        for rom in roms {
            if self.is_installed(rom.id) {
                continue;
            }
            let folder = rom_folder(rom, naming);
            let name = sanitize_component(&rom.file_name());
            let mut candidates = vec![PathBuf::from(&folder).join(&name)];
            if rom.is_multi() {
                candidates.push(PathBuf::from(&folder).join(format!("{name}.zip")));
            }
            if let Some(rel) = candidates.into_iter().find(|c| self.root.join(c).exists()) {
                let size = dir_size(&self.root.join(&rel));
                self.manifest.roms.insert(rom.id, entry_for(rom, rel, size));
                adopted += 1;
            }
        }
        if adopted > 0 {
            self.changed()?;
        }
        Ok(adopted)
    }

    /// Moves installed roms so they match `naming`. Returns how many moved.
    pub fn reorganize(&mut self, naming: FolderNaming) -> Result<(usize, usize)> {
        let mut moved = 0;
        let mut skipped = 0;
        let entries: Vec<InstalledRom> = self.manifest.roms.values().cloned().collect();
        for mut e in entries {
            let folder = platform_folder(naming, &e.platform_fs_slug, &e.platform_slug, &e.platform_name);
            let Some(leaf) = e.rel_path.file_name() else { continue };
            let new_rel = PathBuf::from(&folder).join(leaf);
            if new_rel == e.rel_path {
                continue;
            }
            let from = self.root.join(&e.rel_path);
            let to = self.root.join(&new_rel);
            if to.exists() || !from.exists() {
                skipped += 1;
                continue;
            }
            fs::create_dir_all(self.root.join(&folder))?;
            fs::rename(&from, &to).with_context(|| format!("Could not move {}", from.display()))?;
            if let Some(parent) = from.parent() {
                self.remove_if_empty(parent);
            }
            e.rel_path = new_rel;
            self.manifest.roms.insert(e.rom_id, e);
            moved += 1;
        }
        if moved > 0 {
            self.changed()?;
        }
        Ok((moved, skipped))
    }
}

fn entry_for(rom: &Rom, rel_path: PathBuf, size: u64) -> InstalledRom {
    InstalledRom {
        rom_id: rom.id,
        name: rom.title().to_string(),
        platform_id: rom.platform_id,
        platform_slug: rom.platform_slug.clone(),
        platform_fs_slug: rom.platform_fs_slug.clone(),
        platform_name: rom.platform_label().to_string(),
        rel_path,
        size_bytes: size,
        installed_at: now_ts(),
    }
}

fn extract_zip(zip_path: &Path, target: &Path, progress: &Progress) -> Result<()> {
    let file = fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).context("Downloaded archive is invalid")?;
    fs::create_dir_all(target)?;
    for i in 0..archive.len() {
        if progress.cancelled() {
            bail!(CANCELLED);
        }
        let mut entry = archive.by_index(i)?;
        let Some(rel) = entry.enclosed_name() else { continue };
        let out = target.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out)?;
        } else {
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut f = fs::File::create(&out)?;
            std::io::copy(&mut entry, &mut f)?;
        }
    }
    // If the archive wrapped everything in a single folder of the same name,
    // flatten it so we don't end up with `Game/Game/...`.
    let children: Vec<_> = fs::read_dir(target)?.flatten().collect();
    if children.len() == 1 && children[0].path().is_dir() && children[0].file_name() == target.file_name().unwrap_or_default() {
        let inner = children[0].path();
        let tmp = target.with_extension("flatten-tmp");
        fs::rename(&inner, &tmp)?;
        fs::remove_dir(target)?;
        fs::rename(&tmp, target)?;
    }
    Ok(())
}

/// Downloads `rom` into the library. Runs on a worker thread.
pub fn install(
    client: &RommClient,
    rom: &Rom,
    root: &Path,
    naming: FolderNaming,
    extract_multi: bool,
    progress: &Progress,
) -> Result<InstalledRom> {
    let folder = rom_folder(rom, naming);
    let platform_dir = root.join(&folder);
    fs::create_dir_all(&platform_dir).with_context(|| format!("Could not create {}", platform_dir.display()))?;
    let name = sanitize_component(&rom.file_name());

    let rel = if rom.is_multi() {
        let tmp = platform_dir.join(format!(".{name}.zip.part"));
        client.download_rom(rom, &tmp, progress)?;
        if extract_multi {
            progress.stage.store(STAGE_EXTRACTING, Ordering::Relaxed);
            let target = platform_dir.join(&name);
            if target.exists() {
                fs::remove_dir_all(&target)?;
            }
            let res = extract_zip(&tmp, &target, progress);
            let _ = fs::remove_file(&tmp);
            if let Err(e) = res {
                let _ = fs::remove_dir_all(&target);
                return Err(e);
            }
            PathBuf::from(&folder).join(&name)
        } else {
            let final_name = format!("{name}.zip");
            let dest = platform_dir.join(&final_name);
            let _ = fs::remove_file(&dest);
            fs::rename(&tmp, &dest)?;
            PathBuf::from(&folder).join(final_name)
        }
    } else {
        let tmp = platform_dir.join(format!("{name}.part"));
        client.download_rom(rom, &tmp, progress)?;
        let dest = platform_dir.join(&name);
        let _ = fs::remove_file(&dest);
        fs::rename(&tmp, &dest)?;
        PathBuf::from(&folder).join(&name)
    };

    let size = dir_size(&root.join(&rel));
    Ok(entry_for(rom, rel, size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Auth, Progress, RommClient};

    #[test]
    fn folder_names() {
        assert_eq!(platform_folder(FolderNaming::FsSlug, "snes", "snes", "Super Nintendo"), "snes");
        assert_eq!(platform_folder(FolderNaming::DisplayName, "md", "genesis", "Sega Mega Drive/Genesis"), "Sega Mega Drive_Genesis");
        assert_eq!(platform_folder(FolderNaming::Slug, "", "", ""), "unknown");
    }

    /// Run with a mock or real server:
    /// `ROMM_TEST_URL=http://127.0.0.1:8765 ROMM_TEST_USER=admin ROMM_TEST_PASS=admin cargo test -- --ignored`
    #[test]
    #[ignore]
    fn install_uninstall_roundtrip() {
        let url = std::env::var("ROMM_TEST_URL").expect("ROMM_TEST_URL");
        let auth = Auth::Basic {
            username: std::env::var("ROMM_TEST_USER").unwrap_or_default(),
            password: std::env::var("ROMM_TEST_PASS").unwrap_or_default(),
        };
        let client = RommClient::new(&url, auth, false).unwrap();
        client.current_user().unwrap();
        let (roms, _, _, _) = client.roms_page(0, 500).unwrap();
        let root = std::env::temp_dir().join(format!("romm-explorer-test-{}", std::process::id()));
        let mut lib = Library::open(&root).unwrap();

        let single = roms.iter().find(|r| !r.is_multi()).unwrap();
        let multi = roms.iter().find(|r| r.is_multi());
        for rom in std::iter::once(single).chain(multi) {
            let e = install(&client, rom, &root, FolderNaming::FsSlug, true, &Progress::default()).unwrap();
            assert!(root.join(&e.rel_path).exists());
            assert!(e.size_bytes > 0);
            lib.record(e).unwrap();
        }
        // Reorganise to display names and back.
        let (moved, _) = lib.reorganize(FolderNaming::DisplayName).unwrap();
        assert!(moved >= 1);
        assert!(lib.iter().all(|e| root.join(&e.rel_path).exists()));
        lib.reorganize(FolderNaming::FsSlug).unwrap();

        // Forget and re-detect.
        let ids: Vec<i64> = lib.iter().map(|e| e.rom_id).collect();
        let mut fresh = Library { root: root.clone(), manifest: Manifest::default(), generation: 0 };
        assert_eq!(fresh.adopt_existing(&roms, FolderNaming::FsSlug).unwrap(), ids.len());

        for id in ids {
            lib.uninstall(id).unwrap();
        }
        assert_eq!(lib.count(), 0);
        let leftovers: Vec<_> = std::fs::read_dir(&root).unwrap().flatten().map(|e| e.file_name()).collect();
        assert!(leftovers.iter().all(|n| n.to_string_lossy().starts_with(".romm-explorer")), "{leftovers:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Read-only smoke test against the server in `.env`; downloads the smallest
    /// single-file rom into a temp folder and removes it again.
    /// `cargo test real_server -- --ignored --nocapture`
    #[test]
    #[ignore]
    #[cfg(debug_assertions)]
    fn real_server_smoke() {
        let env = crate::config::load_dotenv().expect(".env with server details");
        let auth = if env.token.is_empty() {
            Auth::Basic { username: env.user.clone(), password: env.password.clone() }
        } else {
            Auth::Token(env.token.clone())
        };
        let client = RommClient::new(&env.server, auth, false).unwrap();
        let user = client.current_user().unwrap();
        let platforms = client.platforms().unwrap();
        let mut roms = Vec::new();
        let mut offset = 0;
        loop {
            let (page, total, raw, paginated) = client.roms_page(offset, 500).unwrap();
            assert_eq!(page.len(), raw, "some roms failed to deserialize");
            roms.extend(page);
            offset += raw as u64;
            if !paginated || raw < 500 || total.is_some_and(|t| offset >= t) {
                break;
            }
        }
        println!("user={user} platforms={} roms={}", platforms.len(), roms.len());
        let multi = roms.iter().filter(|r| r.is_multi()).count();
        let with_cover = roms.iter().filter(|r| client.cover_url(r, false).is_some()).count();
        println!("multi-file={multi} with_cover={with_cover}");

        if let Some(r) = roms.iter().find(|r| client.cover_url(r, false).is_some()) {
            let url = client.cover_url(r, false).unwrap();
            let bytes = client.fetch_bytes(&url).unwrap();
            image::load_from_memory(&bytes).expect("cover decodes");
            println!("cover ok: {url} ({} bytes)", bytes.len());
        }
        if let Some(r) = roms.iter().find(|r| r.release_ts().is_some()) {
            println!("metadata sample: {} year={:?} genres={:?} rating={:?}", r.title(), r.release_ts().and_then(crate::util::year_from_ts), r.genres(), r.rating());
        }

        let small = roms
            .iter()
            .filter(|r| !r.is_multi() && r.fs_size_bytes > 0 && r.fs_size_bytes < 64 << 20)
            .min_by_key(|r| r.fs_size_bytes)
            .expect("a small single-file rom");
        let root = std::env::temp_dir().join(format!("romm-explorer-real-{}", std::process::id()));
        let mut lib = Library::open(&root).unwrap();
        let e = install(&client, small, &root, FolderNaming::FsSlug, true, &Progress::default()).unwrap();
        println!("installed {} -> {} ({} bytes, expected {})", small.title(), e.rel_path.display(), e.size_bytes, small.fs_size_bytes);
        assert_eq!(e.size_bytes, small.fs_size_bytes);
        let id = e.rom_id;
        lib.record(e).unwrap();
        lib.uninstall(id).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }
}
