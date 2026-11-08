use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CornerRadius, CursorIcon, Id, Layout, Margin, Rect, RichText, Sense,
    Stroke, TextureHandle, Ui,
};

use crate::api::{Platform, Progress, Rom, RommClient, CANCELLED, STAGE_EXTRACTING, STAGE_QUEUED};
use crate::config::{Config, ViewMode};
use crate::covers::CoverLoader;
use crate::library::{self, FolderNaming, InstalledRom, Library};
use crate::theme::*;
use crate::util::*;
use crate::widgets::*;

const PAGE_SIZE: u64 = 500;
const DOWNLOAD_WORKERS: usize = 2;
const DETAIL_MIN_W: f32 = 320.0;
const DETAIL_MAX_W: f32 = 760.0;

enum Msg {
    Connected(Result<(RommClient, String), String>),
    Platforms(Result<Vec<Platform>, String>),
    RomsPage { gen: u64, roms: Vec<Rom>, total: Option<u64>, done: bool },
    RomsFailed { gen: u64, err: String },
    InstallDone { rom_id: i64, result: Result<InstalledRom, String> },
}

#[derive(PartialEq, Clone, Copy, Debug)]
enum Nav {
    All,
    Installed,
    Platform(i64),
}

#[derive(PartialEq, Clone, Copy, Debug)]
enum StatusFilter {
    All,
    Installed,
    Available,
}

#[derive(PartialEq, Clone, Copy, Debug)]
enum SortBy {
    Name,
    Platform,
    Size,
    Release,
    Recent,
}

impl SortBy {
    fn label(&self) -> &'static str {
        match self {
            SortBy::Name => "Name",
            SortBy::Platform => "Platform",
            SortBy::Size => "Size",
            SortBy::Release => "Release date",
            SortBy::Recent => "Recently installed",
        }
    }
}

enum ConnState {
    Idle,
    Connecting,
    Connected { user: String },
    Failed(String),
}

#[derive(PartialEq)]
enum DlState {
    Running,
    Done,
    Failed(String),
    Cancelled,
}

struct DownloadEntry {
    rom_id: i64,
    title: String,
    platform: String,
    progress: Arc<Progress>,
    state: DlState,
    last_bytes: u64,
    last_tick: Instant,
    speed: f64,
    finished_at: Option<Instant>,
}

struct Job {
    rom: Rom,
    client: RommClient,
    root: PathBuf,
    naming: FolderNaming,
    extract: bool,
    progress: Arc<Progress>,
}

enum Action {
    Select(i64),
    Install(i64),
    AskUninstall(i64),
    Uninstall(i64),
    Cancel(i64),
    Reveal(i64),
    OpenWeb(i64),
}

#[derive(Clone, Copy, PartialEq)]
enum ToastKind {
    Info,
    Success,
    Error,
}

struct Toast {
    text: String,
    kind: ToastKind,
    at: Instant,
}

type FilterKey = (String, Nav, StatusFilter, SortBy, usize, u64);

pub struct App {
    cfg: Config,
    client: Option<RommClient>,
    library: Option<Library>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    job_tx: Sender<Job>,
    conn: ConnState,

    platforms: Vec<Platform>,
    roms: Vec<Rom>,
    rom_index: HashMap<i64, usize>,
    search_keys: Vec<String>,
    title_keys: Vec<String>,
    platform_counts: HashMap<i64, usize>,
    roms_loading: bool,
    roms_total: Option<u64>,
    load_gen: u64,

    nav: Nav,
    status: StatusFilter,
    sort: SortBy,
    search: String,
    selected: Option<i64>,
    filtered: Vec<usize>,
    filter_key: Option<FilterKey>,
    scroll_to_top: bool,

    covers: CoverLoader,
    downloads: Vec<DownloadEntry>,
    downloads_open: bool,
    actions: Vec<Action>,
    toasts: Vec<Toast>,

    show_settings: bool,
    draft: Config,
    confirm_uninstall: Option<i64>,
}

fn spawn_download_workers(rx: Receiver<Job>, tx: Sender<Msg>, ctx: egui::Context) {
    let rx = Arc::new(Mutex::new(rx));
    for _ in 0..DOWNLOAD_WORKERS {
        let rx = rx.clone();
        let tx = tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || loop {
            let job = {
                let guard = rx.lock().unwrap();
                match guard.recv() {
                    Ok(j) => j,
                    Err(_) => return,
                }
            };
            let result = if job.progress.cancelled() {
                Err(CANCELLED.to_string())
            } else {
                job.progress.stage.store(crate::api::STAGE_DOWNLOADING, Ordering::Relaxed);
                library::install(&job.client, &job.rom, &job.root, job.naming, job.extract, &job.progress)
                    .map_err(|e| format!("{e:#}"))
            };
            let _ = tx.send(Msg::InstallDone { rom_id: job.rom.id, result });
            ctx.request_repaint();
        });
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_fonts(&cc.egui_ctx);
        apply(&cc.egui_ctx);
        let mut app = Self::with_config(&cc.egui_ctx, Config::load());
        if !app.cfg.server_url.trim().is_empty() {
            app.connect(&cc.egui_ctx);
        }
        app
    }

    fn with_config(ctx: &egui::Context, cfg: Config) -> Self {
        let (tx, rx) = channel();
        let (job_tx, job_rx) = channel();
        spawn_download_workers(job_rx, tx.clone(), ctx.clone());
        let covers = CoverLoader::new(ctx, 6, crate::config::cache_dir().join("covers"));

        let mut toasts = Vec::new();
        let library = cfg.library_dir.as_ref().and_then(|d| match Library::open(d) {
            Ok(l) => Some(l),
            Err(e) => {
                toasts.push(Toast { text: format!("Library: {e:#}"), kind: ToastKind::Error, at: Instant::now() });
                None
            }
        });

        Self {
            draft: cfg.clone(),
            cfg,
            client: None,
            library,
            tx,
            rx,
            job_tx,
            conn: ConnState::Idle,
            platforms: Vec::new(),
            roms: Vec::new(),
            rom_index: HashMap::new(),
            search_keys: Vec::new(),
            title_keys: Vec::new(),
            platform_counts: HashMap::new(),
            roms_loading: false,
            roms_total: None,
            load_gen: 0,
            nav: Nav::All,
            status: StatusFilter::All,
            sort: SortBy::Name,
            search: String::new(),
            selected: None,
            filtered: Vec::new(),
            filter_key: None,
            scroll_to_top: false,
            covers,
            downloads: Vec::new(),
            downloads_open: true,
            actions: Vec::new(),
            toasts,
            show_settings: false,
            confirm_uninstall: None,
        }
    }

    // ───────────────────────────── helpers ─────────────────────────────

    fn toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toasts.push(Toast { text: text.into(), kind, at: Instant::now() });
    }

    fn save_cfg(&mut self) {
        if let Err(e) = self.cfg.save() {
            self.toast(ToastKind::Error, format!("Could not save settings: {e:#}"));
        }
    }

    fn rom(&self, id: i64) -> Option<&Rom> {
        self.rom_index.get(&id).map(|i| &self.roms[*i])
    }

    fn is_installed(&self, id: i64) -> bool {
        self.library.as_ref().is_some_and(|l| l.is_installed(id))
    }

    fn active_download(&self, id: i64) -> Option<&DownloadEntry> {
        self.downloads.iter().find(|d| d.rom_id == id && d.state == DlState::Running)
    }

    fn platform_name(&self, rom: &Rom) -> String {
        let label = rom.platform_label();
        if !label.is_empty() {
            return label.to_string();
        }
        self.platforms
            .iter()
            .find(|p| p.id == rom.platform_id)
            .map(|p| p.label().to_string())
            .unwrap_or_default()
    }

    // ─────────────────────────── networking ────────────────────────────

    fn connect(&mut self, ctx: &egui::Context) {
        self.conn = ConnState::Connecting;
        let cfg = self.cfg.clone();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = RommClient::new(&cfg.server_url, cfg.auth(), cfg.accept_invalid_certs)
                .and_then(|c| c.current_user().map(|u| (c, u)))
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::Connected(r));
            ctx.request_repaint();
        });
    }

    fn reload(&mut self, ctx: &egui::Context) {
        let Some(client) = self.client.clone() else { return };

        let tx = self.tx.clone();
        let c2 = client.clone();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Platforms(c2.platforms().map_err(|e| format!("{e:#}"))));
            ctx2.request_repaint();
        });

        self.load_gen += 1;
        let gen = self.load_gen;
        self.roms.clear();
        self.rom_index.clear();
        self.search_keys.clear();
        self.title_keys.clear();
        self.platform_counts.clear();
        // Stale indices into `roms` must never outlive the list they point into.
        self.filtered.clear();
        self.filter_key = None;
        self.roms_loading = true;
        self.roms_total = None;

        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut offset = 0u64;
            loop {
                match client.roms_page(offset, PAGE_SIZE) {
                    Ok((roms, total, raw, paginated)) => {
                        let fetched = offset + raw as u64;
                        let done = !paginated || raw == 0 || (raw as u64) < PAGE_SIZE || total.is_some_and(|t| fetched >= t);
                        let _ = tx.send(Msg::RomsPage { gen, roms, total, done });
                        ctx.request_repaint();
                        if done {
                            break;
                        }
                        offset = fetched;
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::RomsFailed { gen, err: format!("{e:#}") });
                        ctx.request_repaint();
                        break;
                    }
                }
            }
        });
    }

    fn process_messages(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Connected(Ok((client, user))) => {
                    self.covers.set_client(Some(client.clone()));
                    self.client = Some(client);
                    self.conn = ConnState::Connected { user };
                    self.save_cfg();
                    self.reload(ctx);
                }
                Msg::Connected(Err(e)) => {
                    self.conn = ConnState::Failed(e);
                }
                Msg::Platforms(Ok(mut p)) => {
                    p.sort_by_key(|p| p.label().to_lowercase());
                    self.platforms = p;
                }
                Msg::Platforms(Err(e)) => self.toast(ToastKind::Error, format!("Could not load platforms: {e}")),
                Msg::RomsPage { gen, roms, total, done } if gen == self.load_gen => {
                    for rom in roms {
                        if self.rom_index.contains_key(&rom.id) {
                            continue;
                        }
                        let title = rom.title().to_lowercase();
                        self.search_keys.push(format!(
                            "{} {} {} {}",
                            title,
                            rom.fs_name.to_lowercase(),
                            rom.platform_label().to_lowercase(),
                            rom.platform_slug.to_lowercase()
                        ));
                        self.title_keys.push(title);
                        *self.platform_counts.entry(rom.platform_id).or_default() += 1;
                        self.rom_index.insert(rom.id, self.roms.len());
                        self.roms.push(rom);
                    }
                    self.roms_total = total;
                    if done {
                        self.roms_loading = false;
                    }
                }
                Msg::RomsFailed { gen, err } if gen == self.load_gen => {
                    self.roms_loading = false;
                    self.toast(ToastKind::Error, format!("Could not load library: {err}"));
                }
                Msg::RomsPage { .. } | Msg::RomsFailed { .. } => {}
                Msg::InstallDone { rom_id, result } => self.finish_install(rom_id, result),
            }
        }
    }

    fn finish_install(&mut self, rom_id: i64, result: Result<InstalledRom, String>) {
        let entry_idx = self.downloads.iter().position(|d| d.rom_id == rom_id && d.state == DlState::Running);
        let title = entry_idx.map(|i| self.downloads[i].title.clone()).unwrap_or_default();
        let state = match result {
            Ok(installed) => {
                let size = installed.size_bytes;
                match self.library.as_mut().map(|l| l.record(installed)) {
                    Some(Ok(())) => {
                        self.toast(ToastKind::Success, format!("Installed {title}  ·  {}", format_bytes(size)));
                        DlState::Done
                    }
                    Some(Err(e)) => {
                        let msg = format!("Installed but could not update library manifest: {e:#}");
                        self.toast(ToastKind::Error, msg.clone());
                        DlState::Failed(msg)
                    }
                    None => DlState::Failed("No library folder".into()),
                }
            }
            Err(e) if e == CANCELLED => DlState::Cancelled,
            Err(e) => {
                self.toast(ToastKind::Error, format!("{title}: {e}"));
                DlState::Failed(e)
            }
        };
        if let Some(i) = entry_idx {
            self.downloads[i].state = state;
            self.downloads[i].finished_at = Some(Instant::now());
        }
    }

    // ───────────────────────────── actions ─────────────────────────────

    fn apply_actions(&mut self) {
        let actions = std::mem::take(&mut self.actions);
        for a in actions {
            match a {
                Action::Select(id) => self.selected = Some(id),
                Action::Install(id) => self.start_install(id),
                Action::AskUninstall(id) => self.confirm_uninstall = Some(id),
                Action::Uninstall(id) => {
                    let name = self.rom(id).map(|r| r.title().to_string()).unwrap_or_default();
                    if let Some(lib) = self.library.as_mut() {
                        match lib.uninstall(id) {
                            Ok(()) => self.toast(ToastKind::Info, format!("Uninstalled {name}")),
                            Err(e) => self.toast(ToastKind::Error, format!("Uninstall failed: {e:#}")),
                        }
                    }
                }
                Action::Cancel(id) => {
                    if let Some(d) = self.active_download(id) {
                        d.progress.cancel.store(true, Ordering::Relaxed);
                    }
                }
                Action::Reveal(id) => {
                    if let Some(lib) = &self.library {
                        if let Some(e) = lib.get(id) {
                            reveal_in_file_manager(&lib.abs_path(e));
                        }
                    }
                }
                Action::OpenWeb(id) => {
                    if let Some(c) = &self.client {
                        let _ = open::that(c.rom_web_url(id));
                    }
                }
            }
        }
    }

    fn start_install(&mut self, id: i64) {
        if self.is_installed(id) || self.active_download(id).is_some() {
            return;
        }
        let (Some(client), Some(lib)) = (self.client.clone(), self.library.as_ref()) else {
            self.toast(ToastKind::Error, "Choose a library folder in Settings first");
            self.open_settings();
            return;
        };
        let Some(rom) = self.rom(id).cloned() else { return };
        let progress = Arc::new(Progress::default());
        progress.total.store(rom.fs_size_bytes, Ordering::Relaxed);
        let job = Job {
            client,
            root: lib.root.clone(),
            naming: self.cfg.folder_naming,
            extract: self.cfg.extract_multi_file,
            progress: progress.clone(),
            rom: rom.clone(),
        };
        self.downloads.retain(|d| d.rom_id != id);
        self.downloads.push(DownloadEntry {
            rom_id: id,
            title: rom.title().to_string(),
            platform: self.platform_name(&rom),
            progress,
            state: DlState::Running,
            last_bytes: 0,
            last_tick: Instant::now(),
            speed: 0.0,
            finished_at: None,
        });
        self.downloads_open = true;
        let _ = self.job_tx.send(job);
    }

    fn open_settings(&mut self) {
        self.draft = self.cfg.clone();
        self.show_settings = true;
    }

    fn set_library_dir(&mut self, dir: PathBuf) {
        match Library::open(&dir) {
            Ok(lib) => {
                self.library = Some(lib);
                self.cfg.library_dir = Some(dir);
                self.save_cfg();
                self.filter_key = None;
            }
            Err(e) => self.toast(ToastKind::Error, format!("{e:#}")),
        }
    }

    fn update_download_stats(&mut self) {
        let now = Instant::now();
        for d in &mut self.downloads {
            if d.state != DlState::Running {
                continue;
            }
            let dt = now.duration_since(d.last_tick).as_secs_f64();
            if dt >= 0.5 {
                let bytes = d.progress.downloaded.load(Ordering::Relaxed);
                let inst = bytes.saturating_sub(d.last_bytes) as f64 / dt;
                d.speed = if d.speed == 0.0 { inst } else { d.speed * 0.6 + inst * 0.4 };
                d.last_bytes = bytes;
                d.last_tick = now;
            }
        }
        // Successful downloads fade out of the tray after a while.
        self.downloads.retain(|d| {
            !(d.state == DlState::Done || d.state == DlState::Cancelled)
                || d.finished_at.is_none_or(|t| now.duration_since(t) < Duration::from_secs(8))
        });
    }

    fn refresh_filter(&mut self) {
        let gen = self.library.as_ref().map_or(0, |l| l.generation);
        let key: FilterKey = (self.search.clone(), self.nav, self.status, self.sort, self.roms.len(), gen);
        if self.filter_key.as_ref() == Some(&key) {
            return;
        }
        if self.filter_key.as_ref().is_some_and(|k| k.0 != key.0 || k.1 != key.1 || k.2 != key.2 || k.3 != key.3) {
            self.scroll_to_top = true;
        }
        let terms: Vec<String> = self.search.to_lowercase().split_whitespace().map(str::to_owned).collect();
        let lib = self.library.as_ref();
        let installed = |id: i64| lib.is_some_and(|l| l.is_installed(id));

        let mut out: Vec<usize> = (0..self.roms.len())
            .filter(|&i| {
                let r = &self.roms[i];
                let nav_ok = match self.nav {
                    Nav::All => true,
                    Nav::Installed => installed(r.id),
                    Nav::Platform(p) => r.platform_id == p,
                };
                let status_ok = match self.status {
                    StatusFilter::All => true,
                    StatusFilter::Installed => installed(r.id),
                    StatusFilter::Available => !installed(r.id),
                };
                nav_ok && status_ok && terms.iter().all(|t| self.search_keys[i].contains(t.as_str()))
            })
            .collect();

        let roms = &self.roms;
        let titles = &self.title_keys;
        match self.sort {
            SortBy::Name => out.sort_by(|a, b| titles[*a].cmp(&titles[*b])),
            SortBy::Platform => out.sort_by(|a, b| {
                roms[*a].platform_label().cmp(roms[*b].platform_label()).then_with(|| titles[*a].cmp(&titles[*b]))
            }),
            SortBy::Size => out.sort_by(|a, b| roms[*b].fs_size_bytes.cmp(&roms[*a].fs_size_bytes)),
            SortBy::Release => out.sort_by(|a, b| {
                let ra = roms[*a].release_ts().unwrap_or(i64::MIN);
                let rb = roms[*b].release_ts().unwrap_or(i64::MIN);
                rb.cmp(&ra)
            }),
            SortBy::Recent => out.sort_by(|a, b| {
                let ta = lib.and_then(|l| l.get(roms[*a].id)).map_or(0, |e| e.installed_at);
                let tb = lib.and_then(|l| l.get(roms[*b].id)).map_or(0, |e| e.installed_at);
                tb.cmp(&ta).then_with(|| titles[*a].cmp(&titles[*b]))
            }),
        }
        self.filtered = out;
        self.filter_key = Some(key);
    }

    // ─────────────────────────────── UI ────────────────────────────────

    fn login_screen(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG)).show(ctx, |ui| {
            let avail = ui.available_size();
            ui.add_space((avail.y * 0.5 - 300.0).max(24.0));
            ui.vertical_centered(|ui| {
                logo(ui, 56.0);
                ui.add_space(10.0);
                ui.label(RichText::new("RomM Explorer").font(bold(30.0)).color(TEXT));
                ui.label(RichText::new("Browse your RomM collection and build a tidy local library").color(MUTED));
                ui.add_space(22.0);

                egui::Frame::new()
                    .fill(PANEL)
                    .stroke(Stroke::new(1.0_f32, BORDER))
                    .corner_radius(16.0)
                    .inner_margin(Margin::same(26))
                    .show(ui, |ui| {
                        ui.set_width(400.0);
                        ui.with_layout(Layout::top_down(Align::Min), |ui| {
                            let connecting = matches!(self.conn, ConnState::Connecting);
                            let mut submit = server_fields(ui, &mut self.cfg);
                            ui.add_space(6.0);
                            library_field(ui, &mut self.cfg.library_dir);
                            ui.add_space(14.0);

                            if let ConnState::Failed(e) = &self.conn {
                                egui::Frame::new()
                                    .fill(DANGER.gamma_multiply(0.12))
                                    .stroke(Stroke::new(1.0_f32, DANGER.gamma_multiply(0.4)))
                                    .corner_radius(8.0)
                                    .inner_margin(Margin::same(10))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.label(RichText::new(e).color(lighten(DANGER, 0.4)).size(13.0));
                                    });
                                ui.add_space(10.0);
                            }

                            ui.add_enabled_ui(!connecting, |ui| {
                                let label = if connecting { "Connecting…" } else { "Connect" };
                                submit |= primary_button(ui, label, ui.available_width(), 42.0).clicked();
                            });
                            if submit && !connecting {
                                if let Some(dir) = self.cfg.library_dir.clone() {
                                    self.set_library_dir(dir);
                                }
                                self.connect(ui.ctx());
                            }
                        });
                    });
                ui.add_space(12.0);
                ui.label(
                    RichText::new("Use your RomM username & password, or a client API token (rmm_…).")
                        .size(12.0)
                        .color(MUTED),
                );
            });
        });
    }

    fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top")
            .exact_height(62.0)
            .frame(egui::Frame::new().fill(BG).inner_margin(Margin::symmetric(16, 0)).stroke(Stroke::new(1.0_f32, BORDER)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    logo(ui, 32.0);
                    ui.label(RichText::new("RomM Explorer").font(bold(17.0)).color(TEXT));
                    ui.add_space(18.0);

                    let search = egui::TextEdit::singleline(&mut self.search)
                        .hint_text("🔍  Search games, files, platforms…   (Ctrl+F)")
                        .desired_width(360.0)
                        .margin(Margin::symmetric(12, 7));
                    let resp = ui.add(search);
                    if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::F)) {
                        resp.request_focus();
                    }
                    if !self.search.is_empty() && resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        self.search.clear();
                    }
                    ui.add_space(8.0);
                    segmented(
                        ui,
                        &mut self.status,
                        &[
                            (StatusFilter::All, "All"),
                            (StatusFilter::Installed, "Installed"),
                            (StatusFilter::Available, "Not installed"),
                        ],
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, "⚙", "Settings").clicked() {
                            self.open_settings();
                        }
                        if icon_button(ui, "⟳", "Refresh from server  (F5)").clicked() || ui.input(|i| i.key_pressed(egui::Key::F5)) {
                            self.reload(ui.ctx());
                        }
                        let mut view = self.cfg.view_mode;
                        if segmented(ui, &mut view, &[(ViewMode::List, "☰"), (ViewMode::Grid, "▦")]) {
                            self.cfg.view_mode = view;
                            self.save_cfg();
                        }
                        egui::ComboBox::from_id_salt("sort")
                            .selected_text(format!("Sort: {}", self.sort.label()))
                            .width(170.0)
                            .show_ui(ui, |ui| {
                                for s in [SortBy::Name, SortBy::Platform, SortBy::Size, SortBy::Release, SortBy::Recent] {
                                    ui.selectable_value(&mut self.sort, s, s.label());
                                }
                            });
                        if self.cfg.view_mode == ViewMode::Grid {
                            let mut w = self.cfg.card_width;
                            let slider = egui::Slider::new(&mut w, 120.0..=260.0).show_value(false);
                            let resp = ui
                                .scope(|ui| {
                                    ui.spacing_mut().interact_size.y = 14.0;
                                    ui.spacing_mut().slider_width = 90.0;
                                    ui.add(slider).on_hover_text("Cover size")
                                })
                                .inner;
                            if resp.changed() {
                                self.cfg.card_width = w;
                            }
                            if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                                self.save_cfg();
                            }
                        }
                    });
                });
            });
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status")
            .exact_height(30.0)
            .frame(egui::Frame::new().fill(BG).inner_margin(Margin::symmetric(14, 0)).stroke(Stroke::new(1.0_f32, BORDER)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    let (dot, text) = match &self.conn {
                        ConnState::Connected { user } => (
                            SUCCESS,
                            format!("{user} @ {}", self.client.as_ref().map(|c| c.host()).unwrap_or_default()),
                        ),
                        ConnState::Connecting => (WARN, "Connecting…".into()),
                        ConnState::Failed(_) => (DANGER, "Disconnected".into()),
                        ConnState::Idle => (MUTED, "Not connected".into()),
                    };
                    let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 4.0, dot);
                    ui.label(RichText::new(text).size(12.0).color(TEXT_DIM));

                    ui.add_space(12.0);
                    match &self.library {
                        Some(lib) => {
                            let resp = ui
                                .add(
                                    egui::Label::new(
                                        RichText::new(format!("📁 {}", lib.root.display())).size(12.0).color(TEXT_DIM),
                                    )
                                    .sense(Sense::click()),
                                )
                                .on_hover_text("Open library folder")
                                .on_hover_cursor(CursorIcon::PointingHand);
                            if resp.clicked() {
                                let _ = open::that(&lib.root);
                            }
                        }
                        None => {
                            if ui
                                .add(egui::Label::new(RichText::new("⚠ No library folder set").size(12.0).color(WARN)).sense(Sense::click()))
                                .on_hover_cursor(CursorIcon::PointingHand)
                                .clicked()
                            {
                                self.open_settings();
                            }
                        }
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(lib) = &self.library {
                            ui.label(
                                RichText::new(format!(
                                    "{} installed  ·  {}",
                                    format_count(lib.count()),
                                    format_bytes(lib.total_size())
                                ))
                                .size(12.0)
                                .color(TEXT_DIM),
                            );
                        }
                        if self.roms_loading {
                            ui.add(egui::Spinner::new().size(12.0).color(ACCENT_HI));
                            let t = match self.roms_total {
                                Some(t) => format!("Loading library… {} / {}", format_count(self.roms.len()), format_count(t as usize)),
                                None => format!("Loading library… {}", format_count(self.roms.len())),
                            };
                            ui.label(RichText::new(t).size(12.0).color(MUTED));
                        }
                    });
                });
            });
    }

    fn downloads_panel(&mut self, ctx: &egui::Context) {
        if self.downloads.is_empty() {
            return;
        }
        let running = self.downloads.iter().filter(|d| d.state == DlState::Running).count();
        egui::TopBottomPanel::bottom("downloads")
            .resizable(false)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin::symmetric(16, 10)).stroke(Stroke::new(1.0_f32, BORDER)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let arrow = if self.downloads_open { "⏷" } else { "⏵" };
                    if ui
                        .add(egui::Label::new(RichText::new(format!("{arrow}  Downloads")).font(bold(13.0)).color(TEXT)).sense(Sense::click()))
                        .on_hover_cursor(CursorIcon::PointingHand)
                        .clicked()
                    {
                        self.downloads_open = !self.downloads_open;
                    }
                    if running > 0 {
                        chip(ui, &format!("{running} active"), ACCENT);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.downloads.iter().any(|d| d.state != DlState::Running)
                            && ui.add(egui::Button::new(RichText::new("Clear finished").size(12.0)).small()).clicked()
                        {
                            self.downloads.retain(|d| d.state == DlState::Running);
                        }
                    });
                });
                if !self.downloads_open {
                    return;
                }
                ui.add_space(4.0);
                egui::ScrollArea::vertical().max_height(170.0).auto_shrink([false, true]).show(ui, |ui| {
                    for i in 0..self.downloads.len() {
                        self.download_row(ui, i);
                    }
                });
            });
    }

    fn download_row(&mut self, ui: &mut Ui, i: usize) {
        let d = &self.downloads[i];
        let w = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(vec2(w, 40.0), Sense::hover());
        let p = ui.painter().clone();
        p.rect_filled(rect, 8.0, SURFACE);

        let text_w = (w * 0.32).min(360.0);
        let title = truncated(ui, &d.title, bold(13.0), TEXT, text_w);
        p.galley(pos2(rect.left() + 12.0, rect.top() + 5.0), title, TEXT);
        let plat = truncated(ui, &d.platform, regular(11.5), MUTED, text_w);
        p.galley(pos2(rect.left() + 12.0, rect.top() + 22.0), plat, MUTED);

        let frac = d.progress.fraction();
        let stage = d.progress.stage.load(Ordering::Relaxed);
        let downloaded = d.progress.downloaded.load(Ordering::Relaxed);
        let total = d.progress.total.load(Ordering::Relaxed);
        let (status, color) = match &d.state {
            DlState::Running if stage == STAGE_QUEUED => ("Queued".to_string(), MUTED),
            DlState::Running if stage == STAGE_EXTRACTING => ("Extracting…".to_string(), ACCENT_2),
            DlState::Running => {
                let eta = if d.speed > 1.0 && total > downloaded {
                    format!("  ·  {} left", format_duration((total - downloaded) as f64 / d.speed))
                } else {
                    String::new()
                };
                (
                    format!("{} / {}  ·  {}/s{eta}", format_bytes(downloaded), format_bytes(total), format_bytes(d.speed as u64)),
                    TEXT_DIM,
                )
            }
            DlState::Done => ("✔ Installed".to_string(), SUCCESS),
            DlState::Cancelled => ("Cancelled".to_string(), MUTED),
            DlState::Failed(e) => (format!("✖ {e}"), DANGER),
        };

        let bar_left = rect.left() + text_w + 32.0;
        let btn_w = 76.0;
        let bar_right = rect.right() - btn_w - 20.0;
        if bar_right > bar_left + 40.0 {
            let bar = Rect::from_min_max(pos2(bar_left, rect.top() + 9.0), pos2(bar_right, rect.top() + 15.0));
            let (f, c) = match d.state {
                DlState::Done => (1.0, SUCCESS),
                DlState::Failed(_) => (frac, DANGER),
                _ if stage == STAGE_EXTRACTING => (1.0, ACCENT_2),
                _ => (frac, ACCENT),
            };
            progress_bar(&p, bar, f, c);
            let st = truncated(ui, &status, regular(11.5), color, bar_right - bar_left);
            p.galley(pos2(bar_left, rect.top() + 21.0), st, color);
        }

        let btn = Rect::from_min_size(pos2(rect.right() - btn_w - 10.0, rect.center().y - 12.0), vec2(btn_w, 24.0));
        let id = d.rom_id;
        let running = d.state == DlState::Running;
        let failed = matches!(d.state, DlState::Failed(_));
        if running {
            if pill_button(ui, btn, Id::new(("dl-cancel", id)), "Cancel", SURFACE_HI, TEXT_DIM).clicked() {
                self.actions.push(Action::Cancel(id));
            }
        } else if failed {
            if pill_button(ui, btn, Id::new(("dl-retry", id)), "Retry", ACCENT, Color32::WHITE).clicked() {
                self.actions.push(Action::Install(id));
            }
        } else if pill_button(ui, btn, Id::new(("dl-view", id)), "Show", SURFACE_HI, TEXT_DIM).clicked() {
            self.actions.push(Action::Select(id));
        }
        ui.add_space(2.0);
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        let mut installed_by_platform: HashMap<i64, usize> = HashMap::new();
        if let Some(lib) = &self.library {
            for e in lib.iter() {
                *installed_by_platform.entry(e.platform_id).or_default() += 1;
            }
        }
        let installed_total = self.library.as_ref().map_or(0, |l| l.count());

        egui::SidePanel::left("nav")
            .exact_width(256.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin::symmetric(12, 14)).stroke(Stroke::new(1.0_f32, BORDER)))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                section_label(ui, "LIBRARY");
                if nav_item(ui, "🎮", "All games", self.roms.len(), 0, self.nav == Nav::All).clicked() {
                    self.nav = Nav::All;
                }
                if nav_item(ui, "✔", "Installed", installed_total, 0, self.nav == Nav::Installed).clicked() {
                    self.nav = Nav::Installed;
                }
                ui.add_space(14.0);
                section_label(ui, "PLATFORMS");
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    let mut any = false;
                    for p in &self.platforms {
                        let count = self.platform_counts.get(&p.id).copied().unwrap_or(0);
                        let count = if count == 0 && self.roms_loading { p.rom_count.max(0) as usize } else { count };
                        if count == 0 {
                            continue;
                        }
                        any = true;
                        let inst = installed_by_platform.get(&p.id).copied().unwrap_or(0);
                        if nav_item(ui, "", p.label(), count, inst, self.nav == Nav::Platform(p.id)).clicked() {
                            self.nav = Nav::Platform(p.id);
                        }
                    }
                    if !any {
                        ui.add_space(6.0);
                        ui.label(RichText::new("No platforms yet").color(MUTED).size(12.0));
                    }
                });
            });
    }

    fn detail_panel(&mut self, ctx: &egui::Context) {
        let Some(id) = self.selected else { return };
        let Some(rom) = self.rom(id).cloned() else {
            // Keep the selection while a refresh is repopulating the list.
            if !self.roms_loading {
                self.selected = None;
            }
            return;
        };
        let installed = self.library.as_ref().and_then(|l| l.get(id).map(|e| (e.clone(), l.abs_path(e))));
        let dl = self.active_download(id).map(|d| (d.progress.fraction(), d.progress.stage.load(Ordering::Relaxed), d.speed));
        let platform = self.platform_name(&rom);

        let large_url = self.client.as_ref().and_then(|c| c.cover_url(&rom, true));
        let tex = self.covers.get((id, true), large_url).or_else(|| self.covers.peek((id, false)));

        let panel = egui::SidePanel::right("detail")
            .resizable(true)
            .default_width(self.cfg.detail_width.clamp(DETAIL_MIN_W, DETAIL_MAX_W))
            .width_range(DETAIL_MIN_W..=DETAIL_MAX_W)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin::same(18)).stroke(Stroke::new(1.0_f32, BORDER)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    let w = ui.available_width();

                    // Cover: keep the poster's 3:4 shape, centred, however wide the panel is.
                    let h = (w * 4.0 / 3.0).min(560.0);
                    let (row, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
                    let cover_rect = Rect::from_center_size(row.center(), vec2((h * 0.75).min(w), h));
                    paint_cover(ui, cover_rect, tex.as_ref(), CornerRadius::same(14), &rom.platform_fs_slug, true);
                    let close = Rect::from_min_size(cover_rect.right_top() + vec2(-38.0, 8.0), vec2(30.0, 30.0));
                    let close_resp = ui.interact(close, Id::new("close-detail"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
                    ui.painter().circle_filled(close.center(), 15.0, Color32::from_black_alpha(if close_resp.hovered() { 220 } else { 160 }));
                    ui.painter().text(close.center(), Align2::CENTER_CENTER, "✖", regular(13.0), Color32::WHITE);
                    if close_resp.clicked() {
                        self.selected = None;
                    }

                    ui.add_space(14.0);
                    ui.label(RichText::new(rom.title()).font(bold(22.0)).color(TEXT));
                    ui.label(RichText::new(&platform).color(ACCENT_HI).size(14.0));
                    ui.add_space(6.0);

                    let mut chips: Vec<(String, Color32)> = Vec::new();
                    if let Some(y) = rom.release_ts().and_then(year_from_ts) {
                        chips.push((y.to_string(), TEXT_DIM));
                    }
                    for r in rom.regions.iter().take(4) {
                        chips.push((format!("🌐 {r}"), ACCENT_2));
                    }
                    for l in rom.languages.iter().take(4) {
                        chips.push((l.clone(), ACCENT_2));
                    }
                    for g in rom.genres().into_iter().take(4) {
                        chips.push((g, ACCENT));
                    }
                    if !rom.revision.is_empty() {
                        chips.push((format!("Rev {}", rom.revision), WARN));
                    }
                    if !chips.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                            for (t, c) in &chips {
                                chip(ui, t, *c);
                            }
                        });
                    }
                    ui.add_space(14.0);

                    // Primary actions
                    if let Some((frac, stage, speed)) = dl {
                        let (r, _) = ui.allocate_exact_size(vec2(w, 10.0), Sense::hover());
                        let extracting = stage == STAGE_EXTRACTING;
                        progress_bar(ui.painter(), r, if extracting { 1.0 } else { frac }, if extracting { ACCENT_2 } else { ACCENT });
                        let label = if stage == STAGE_QUEUED {
                            "Queued…".to_string()
                        } else if extracting {
                            "Extracting…".to_string()
                        } else {
                            format!("Downloading  {:.0}%  ·  {}/s", frac * 100.0, format_bytes(speed as u64))
                        };
                        ui.label(RichText::new(label).color(TEXT_DIM).size(12.5));
                        if secondary_button(ui, "Cancel", TEXT_DIM, w).clicked() {
                            self.actions.push(Action::Cancel(id));
                        }
                    } else if let Some((entry, path)) = &installed {
                        egui::Frame::new()
                            .fill(SUCCESS.gamma_multiply(0.10))
                            .stroke(Stroke::new(1.0_f32, SUCCESS.gamma_multiply(0.35)))
                            .corner_radius(10.0)
                            .inner_margin(Margin::same(12))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.label(RichText::new("✔  Installed").font(bold(14.0)).color(SUCCESS));
                                ui.label(RichText::new(path.display().to_string()).size(11.5).color(TEXT_DIM));
                                ui.label(
                                    RichText::new(format!(
                                        "{}  ·  {}",
                                        format_bytes(entry.size_bytes),
                                        date_from_ts(entry.installed_at).unwrap_or_default()
                                    ))
                                    .size(11.5)
                                    .color(MUTED),
                                );
                            });
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            let half = (w - 8.0) / 2.0;
                            if secondary_button(ui, "📂  Show in folder", TEXT, half).clicked() {
                                self.actions.push(Action::Reveal(id));
                            }
                            if secondary_button(ui, "🗑  Uninstall", lighten(DANGER, 0.25), half).clicked() {
                                self.actions.push(Action::AskUninstall(id));
                            }
                        });
                    } else {
                        let label = if rom.fs_size_bytes > 0 {
                            format!("⬇   Install   ·   {}", format_bytes(rom.fs_size_bytes))
                        } else {
                            "⬇   Install".to_string()
                        };
                        if primary_button(ui, &label, w, 44.0).clicked() {
                            self.actions.push(Action::Install(id));
                        }
                    }
                    if ui
                        .add(egui::Label::new(RichText::new("🌐  View in RomM").size(12.5).color(ACCENT_HI)).sense(Sense::click()))
                        .on_hover_cursor(CursorIcon::PointingHand)
                        .clicked()
                    {
                        self.actions.push(Action::OpenWeb(id));
                    }

                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(6.0);

                    egui::Grid::new("info").num_columns(2).spacing([18.0, 8.0]).show(ui, |ui| {
                        let mut row = |k: &str, v: String| {
                            if v.is_empty() {
                                return;
                            }
                            ui.label(RichText::new(k).color(MUTED).size(12.5));
                            ui.add(egui::Label::new(RichText::new(v).color(TEXT).size(12.5)).wrap());
                            ui.end_row();
                        };
                        row("Released", rom.release_ts().and_then(date_from_ts).unwrap_or_default());
                        row("Developer", rom.companies().into_iter().take(2).collect::<Vec<_>>().join(", "));
                        row("Rating", rom.rating().map(|r| format!("{:.0} / 100", r)).unwrap_or_default());
                        row("File", rom.fs_name.clone());
                        row("Size", if rom.fs_size_bytes > 0 { format_bytes(rom.fs_size_bytes) } else { String::new() });
                        row("Type", if rom.is_multi() { "Multi-file (folder)".into() } else { "Single file".into() });
                        row(
                            "Saved to",
                            self.library
                                .as_ref()
                                .map(|_| format!("{}/", library::rom_folder(&rom, self.cfg.folder_naming)))
                                .unwrap_or_default(),
                        );
                    });

                    if !rom.summary.is_empty() {
                        ui.add_space(12.0);
                        ui.label(RichText::new("About").font(bold(14.0)).color(TEXT));
                        ui.add_space(2.0);
                        ui.label(RichText::new(&rom.summary).color(TEXT_DIM).size(13.0).line_height(Some(19.0)));
                    }
                    ui.add_space(12.0);
                });
            });

        // Remember the width the user dragged the panel to (saved once they let go).
        let width = panel.response.rect.width();
        if (width - self.cfg.detail_width).abs() > 0.5 && !ctx.input(|i| i.pointer.any_down()) {
            self.cfg.detail_width = width;
            self.save_cfg();
        }
    }

    fn central(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(Margin { left: 20, right: 12, top: 16, bottom: 0 }))
            .show(ctx, |ui| {
                let title = match self.nav {
                    Nav::All => "All games".to_string(),
                    Nav::Installed => "Installed".to_string(),
                    Nav::Platform(p) => self.platforms.iter().find(|x| x.id == p).map(|x| x.label().to_string()).unwrap_or_default(),
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(title).font(bold(24.0)).color(TEXT));
                    ui.add_space(6.0);
                    let n = self.filtered.len();
                    ui.label(RichText::new(format!("{} {}", format_count(n), if n == 1 { "game" } else { "games" })).color(MUTED));
                    if let Nav::Platform(_) = self.nav {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(8.0);
                            let not_installed: Vec<i64> = self
                                .filtered
                                .iter()
                                .filter_map(|i| self.roms.get(*i).map(|r| r.id))
                                .filter(|id| !self.is_installed(*id) && self.active_download(*id).is_none())
                                .collect();
                            if !not_installed.is_empty()
                                && secondary_button(ui, &format!("⬇  Install all {}", not_installed.len()), TEXT, 0.0)
                                    .on_hover_text("Queue every game in this view that isn't installed yet")
                                    .clicked()
                            {
                                self.actions.extend(not_installed.into_iter().map(Action::Install));
                            }
                        });
                    }
                });
                ui.add_space(10.0);

                if self.filtered.is_empty() {
                    self.empty_state(ui);
                    return;
                }
                match self.cfg.view_mode {
                    ViewMode::Grid => self.grid(ui),
                    ViewMode::List => self.list(ui),
                }
            });
    }

    fn empty_state(&mut self, ui: &mut Ui) {
        ui.add_space(ui.available_height() * 0.25);
        ui.vertical_centered(|ui| {
            if self.roms_loading && self.roms.is_empty() {
                ui.add(egui::Spinner::new().size(32.0).color(ACCENT));
                ui.add_space(10.0);
                ui.label(RichText::new("Loading your collection…").color(TEXT_DIM));
                return;
            }
            ui.label(RichText::new("🕹").size(48.0).color(MUTED));
            ui.add_space(8.0);
            let (head, sub) = if !self.search.is_empty() {
                ("No matches", "Try a different search term.")
            } else if self.nav == Nav::Installed || self.status == StatusFilter::Installed {
                ("Nothing installed yet", "Pick a game and hit Install to add it to your library.")
            } else {
                ("No games here", "Your RomM server didn't return any games for this view.")
            };
            ui.label(RichText::new(head).font(bold(18.0)).color(TEXT));
            ui.label(RichText::new(sub).color(MUTED));
        });
    }

    fn grid(&mut self, ui: &mut Ui) {
        let gap = 16.0;
        let avail = ui.available_width() - 10.0;
        let cols = ((avail + gap) / (self.cfg.card_width + gap)).floor().max(1.0) as usize;
        let card_w = ((avail - gap * (cols as f32 - 1.0)) / cols as f32).floor();
        let card_h = card_w * 4.0 / 3.0 + 54.0;
        let rows = self.filtered.len().div_ceil(cols);

        let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("grid");
        if std::mem::take(&mut self.scroll_to_top) {
            area = area.vertical_scroll_offset(0.0);
        }
        ui.spacing_mut().item_spacing = vec2(gap, gap);
        area.show_rows(ui, card_h, rows, |ui, range| {
            for row in range {
                ui.horizontal(|ui| {
                    for c in 0..cols {
                        let k = row * cols + c;
                        if k < self.filtered.len() {
                            let idx = self.filtered[k];
                            self.grid_card(ui, idx, card_w, card_h);
                        }
                    }
                });
            }
            ui.add_space(8.0);
        });
    }

    fn grid_card(&mut self, ui: &mut Ui, idx: usize, w: f32, h: f32) {
        if idx >= self.roms.len() {
            return;
        }
        let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let rom = &self.roms[idx];
        let id = rom.id;
        let installed = self.library.as_ref().is_some_and(|l| l.is_installed(id));
        let dl = self
            .downloads
            .iter()
            .find(|d| d.rom_id == id && d.state == DlState::Running)
            .map(|d| (d.progress.fraction(), d.progress.stage.load(Ordering::Relaxed)));
        let cover_url = self.client.as_ref().and_then(|c| c.cover_url(rom, false));
        let tex = self.covers.get((id, false), cover_url);
        let rom = &self.roms[idx];
        let hovered = ui.rect_contains_pointer(rect);
        let selected = self.selected == Some(id);

        let p = ui.painter().clone();
        let radius = 12.0;
        if hovered {
            // Soft glow.
            p.rect_filled(rect.expand(3.0), radius + 3.0, ACCENT.gamma_multiply(0.10));
        }
        p.rect_filled(rect, radius, if hovered { SURFACE_HI } else { SURFACE });

        let cover = Rect::from_min_size(rect.min, vec2(w, w * 4.0 / 3.0));
        paint_cover(ui, cover, tex.as_ref(), CornerRadius { nw: 12, ne: 12, sw: 0, se: 0 }, &rom.platform_fs_slug, false);

        if installed {
            let badge = Rect::from_min_size(cover.left_top() + vec2(8.0, 8.0), vec2(26.0, 26.0));
            p.circle_filled(badge.center(), 13.0, SUCCESS);
            p.text(badge.center(), Align2::CENTER_CENTER, "✔", bold(13.0), Color32::WHITE);
        }

        if let Some((frac, stage)) = dl {
            p.rect_filled(cover, CornerRadius { nw: 12, ne: 12, sw: 0, se: 0 }, Color32::from_black_alpha(150));
            let text = match stage {
                STAGE_QUEUED => "Queued".to_string(),
                STAGE_EXTRACTING => "Extracting".to_string(),
                _ => format!("{:.0}%", frac * 100.0),
            };
            p.text(cover.center(), Align2::CENTER_CENTER, text, bold(18.0), Color32::WHITE);
            let bar = Rect::from_min_max(pos2(cover.left() + 12.0, cover.bottom() - 16.0), pos2(cover.right() - 12.0, cover.bottom() - 10.0));
            progress_bar(&p, bar, if stage == STAGE_EXTRACTING { 1.0 } else { frac }, ACCENT);
        }

        let title_color = if selected { Color32::WHITE } else { TEXT };
        let title = truncated(ui, rom.title(), bold(13.5), title_color, w - 20.0);
        p.galley(pos2(rect.left() + 10.0, cover.bottom() + 9.0), title, title_color);
        let sub = {
            let plat = rom.platform_label();
            if rom.fs_size_bytes > 0 { format!("{plat}  ·  {}", format_bytes(rom.fs_size_bytes)) } else { plat.to_string() }
        };
        let sub = truncated(ui, &sub, regular(11.5), MUTED, w - 20.0);
        p.galley(pos2(rect.left() + 10.0, cover.bottom() + 29.0), sub, MUTED);

        if selected {
            card_border(&p, rect, radius, ACCENT, 2.0);
        } else if hovered {
            card_border(&p, rect, radius, BORDER_HI, 1.0);
        }

        // Quick install button on hover.
        let mut quick_clicked = false;
        if hovered && !installed && dl.is_none() {
            let b = Rect::from_center_size(cover.right_bottom() - vec2(24.0, 24.0), vec2(36.0, 36.0));
            let br = ui.interact(b, Id::new(("quick", id)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Install");
            let fill = if br.hovered() { ACCENT_HI } else { ACCENT };
            p.circle_filled(b.center() + vec2(0.0, 2.0), 18.0, Color32::from_black_alpha(90));
            p.circle_filled(b.center(), 18.0, fill);
            p.text(b.center(), Align2::CENTER_CENTER, "⬇", bold(16.0), Color32::WHITE);
            if br.clicked() {
                quick_clicked = true;
                self.actions.push(Action::Install(id));
            }
        }

        let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
        if resp.clicked() && !quick_clicked {
            self.actions.push(Action::Select(id));
        }
        if resp.double_clicked() && !installed {
            self.actions.push(Action::Install(id));
        }
        self.context_menu(&resp, id, installed, dl.is_some());
    }

    fn context_menu(&mut self, resp: &egui::Response, id: i64, installed: bool, downloading: bool) {
        resp.context_menu(|ui| {
            ui.set_min_width(180.0);
            if downloading {
                if ui.button("✖  Cancel download").clicked() {
                    self.actions.push(Action::Cancel(id));
                    ui.close();
                }
            } else if installed {
                if ui.button("📂  Show in folder").clicked() {
                    self.actions.push(Action::Reveal(id));
                    ui.close();
                }
                if ui.button("🗑  Uninstall").clicked() {
                    self.actions.push(Action::AskUninstall(id));
                    ui.close();
                }
            } else if ui.button("⬇  Install").clicked() {
                self.actions.push(Action::Install(id));
                ui.close();
            }
            ui.separator();
            if ui.button("ℹ  Details").clicked() {
                self.actions.push(Action::Select(id));
                ui.close();
            }
            if ui.button("🌐  View in RomM").clicked() {
                self.actions.push(Action::OpenWeb(id));
                ui.close();
            }
        });
    }

    fn list(&mut self, ui: &mut Ui) {
        let row_h = 60.0;
        let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("list");
        if std::mem::take(&mut self.scroll_to_top) {
            area = area.vertical_scroll_offset(0.0);
        }
        ui.spacing_mut().item_spacing = vec2(0.0, 4.0);
        area.show_rows(ui, row_h, self.filtered.len(), |ui, range| {
            for k in range {
                let idx = self.filtered[k];
                self.list_row(ui, idx, row_h);
            }
        });
    }

    fn list_row(&mut self, ui: &mut Ui, idx: usize, h: f32) {
        if idx >= self.roms.len() {
            return;
        }
        let w = ui.available_width() - 10.0;
        let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let id = self.roms[idx].id;
        let installed = self.library.as_ref().is_some_and(|l| l.is_installed(id));
        let dl = self
            .downloads
            .iter()
            .find(|d| d.rom_id == id && d.state == DlState::Running)
            .map(|d| (d.progress.fraction(), d.progress.stage.load(Ordering::Relaxed)));
        let cover_url = self.client.as_ref().and_then(|c| c.cover_url(&self.roms[idx], false));
        let tex = self.covers.get((id, false), cover_url);
        let rom = &self.roms[idx];
        let hovered = ui.rect_contains_pointer(rect);
        let selected = self.selected == Some(id);
        let p = ui.painter().clone();

        if selected {
            p.rect_filled(rect, 10.0, ACCENT.gamma_multiply(0.16));
        } else if hovered {
            p.rect_filled(rect, 10.0, SURFACE);
        }

        let thumb = Rect::from_min_size(rect.left_top() + vec2(8.0, 6.0), vec2((h - 12.0) * 0.75, h - 12.0));
        paint_cover(ui, thumb, tex.as_ref(), CornerRadius::same(5), &rom.platform_fs_slug, false);

        let action_w = 110.0;
        let cols_w = w - thumb.right() - action_w - 30.0;
        let name_x = thumb.right() + 14.0;
        let name_w = cols_w * 0.5;
        let title = truncated(ui, rom.title(), bold(14.0), TEXT, name_w);
        p.galley(pos2(name_x, rect.top() + 11.0), title, TEXT);
        let file = truncated(ui, &rom.fs_name, regular(11.5), MUTED, name_w);
        p.galley(pos2(name_x, rect.top() + 32.0), file, MUTED);

        let plat_x = name_x + name_w + 16.0;
        let plat_w = cols_w * 0.27;
        let plat = truncated(ui, rom.platform_label(), regular(13.0), TEXT_DIM, plat_w);
        p.galley(pos2(plat_x, rect.center().y - 9.0), plat, TEXT_DIM);

        let size_x = plat_x + plat_w + 12.0;
        let size_w = cols_w * 0.2;
        let size_text = if rom.fs_size_bytes > 0 { format_bytes(rom.fs_size_bytes) } else { "—".into() };
        let size_g = truncated(ui, &size_text, regular(13.0), TEXT_DIM, size_w);
        p.galley(pos2(size_x, rect.center().y - 9.0), size_g, TEXT_DIM);

        let btn = Rect::from_min_size(pos2(rect.right() - action_w - 8.0, rect.center().y - 14.0), vec2(action_w, 28.0));
        if let Some((frac, stage)) = dl {
            let bar = Rect::from_min_max(pos2(btn.left(), rect.center().y - 3.0), pos2(btn.right(), rect.center().y + 3.0));
            progress_bar(&p, bar, if stage == STAGE_EXTRACTING { 1.0 } else { frac }, ACCENT);
            let label = if stage == STAGE_EXTRACTING { "Extracting".into() } else { format!("{:.0}%", frac * 100.0) };
            p.text(pos2(btn.center().x, bar.top() - 9.0), Align2::CENTER_CENTER, label, regular(11.0), TEXT_DIM);
        } else if installed {
            let r = pill_button(ui, btn, Id::new(("list-inst", id)), "✔ Installed", SUCCESS.gamma_multiply(0.18), SUCCESS);
            if r.clicked() {
                self.actions.push(Action::Select(id));
            }
        } else if pill_button(ui, btn, Id::new(("list-get", id)), "⬇ Install", ACCENT, Color32::WHITE).clicked() {
            self.actions.push(Action::Install(id));
        }

        let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
        if resp.clicked() {
            self.actions.push(Action::Select(id));
        }
        self.context_menu(&resp, id, installed, dl.is_some());
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = true;
        let mut save = false;
        let mut library_ops: Option<&'static str> = None;
        egui::Window::new(RichText::new("Settings").font(bold(18.0)))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .fixed_size(vec2(500.0, 0.0))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                section_label(ui, "ROMM SERVER");
                save |= server_fields(ui, &mut self.draft);
                ui.add_space(10.0);

                section_label(ui, "LOCAL LIBRARY");
                library_field(ui, &mut self.draft.library_dir);
                ui.add_space(4.0);
                ui.label(RichText::new("Platform folder naming").size(12.5).color(TEXT_DIM));
                egui::ComboBox::from_id_salt("naming")
                    .selected_text(self.draft.folder_naming.label())
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for n in FolderNaming::ALL {
                            ui.selectable_value(&mut self.draft.folder_naming, n, n.label());
                        }
                    });
                ui.checkbox(&mut self.draft.extract_multi_file, "Extract multi-file games (discs, folders) into their own folder");
                ui.add_space(6.0);
                ui.label(RichText::new("Library maintenance").size(12.5).color(TEXT_DIM));
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Detect existing files").on_hover_text("Find games already in your library folder and mark them installed").clicked() {
                        library_ops = Some("adopt");
                    }
                    if ui.button("Verify installed").on_hover_text("Forget installs whose files were deleted outside the app").clicked() {
                        library_ops = Some("verify");
                    }
                    if ui.button("Reorganize folders").on_hover_text("Move installed games to match the folder naming above").clicked() {
                        library_ops = Some("reorg");
                    }
                });

                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if primary_button(ui, "Save", 120.0, 36.0).clicked() {
                        save = true;
                    }
                    if secondary_button(ui, "Cancel", TEXT_DIM, 100.0).clicked() {
                        self.show_settings = false;
                    }
                });
            });
        if !open {
            self.show_settings = false;
        }
        if save || library_ops.is_some() {
            self.apply_settings(ctx);
            if save {
                self.show_settings = false;
            }
        }
        if let Some(op) = library_ops {
            self.run_library_op(op);
        }
    }

    fn apply_settings(&mut self, ctx: &egui::Context) {
        let draft = self.draft.clone();
        let reconnect = draft.server_settings_differ(&self.cfg) || !matches!(self.conn, ConnState::Connected { .. });
        let lib_changed = draft.library_dir != self.cfg.library_dir;
        let naming_changed = draft.folder_naming != self.cfg.folder_naming;
        self.cfg = Config {
            view_mode: self.cfg.view_mode,
            card_width: self.cfg.card_width,
            detail_width: self.cfg.detail_width,
            ..draft
        };
        self.save_cfg();
        if lib_changed {
            match self.cfg.library_dir.clone() {
                Some(dir) => self.set_library_dir(dir),
                None => self.library = None,
            }
        }
        if naming_changed && self.library.as_ref().is_some_and(|l| l.count() > 0) {
            self.toast(ToastKind::Info, "Folder naming changed. Use “Reorganize folders” to move existing installs.");
        }
        if reconnect {
            self.client = None;
            self.covers.set_client(None);
            self.connect(ctx);
        }
    }

    fn run_library_op(&mut self, op: &str) {
        let naming = self.cfg.folder_naming;
        let Some(lib) = self.library.as_mut() else {
            self.toast(ToastKind::Error, "Choose a library folder first");
            return;
        };
        let msg = match op {
            "adopt" => lib.adopt_existing(&self.roms, naming).map(|n| format!("Found {n} existing game(s)")),
            "verify" => lib.verify().map(|n| {
                if n == 0 { "All installed games are present".into() } else { format!("Removed {n} missing game(s) from the library") }
            }),
            "reorg" => lib.reorganize(naming).map(|(m, s)| {
                if s > 0 { format!("Moved {m} game(s), skipped {s} (conflict or missing)") } else { format!("Moved {m} game(s)") }
            }),
            _ => return,
        };
        match msg {
            Ok(m) => self.toast(ToastKind::Success, m),
            Err(e) => self.toast(ToastKind::Error, format!("{e:#}")),
        }
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(id) = self.confirm_uninstall else { return };
        let name = self.rom(id).map(|r| r.title().to_string()).unwrap_or_else(|| format!("#{id}"));
        let size = self.library.as_ref().and_then(|l| l.get(id)).map(|e| format_bytes(e.size_bytes)).unwrap_or_default();
        let modal = egui::Modal::new(Id::new("confirm-uninstall")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.label(RichText::new("Uninstall game?").font(bold(18.0)).color(TEXT));
            ui.add_space(4.0);
            ui.label(RichText::new(format!("{name} ({size}) will be deleted from your library folder. You can reinstall it any time from RomM.")).color(TEXT_DIM));
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                let del = egui::Button::new(RichText::new("Uninstall").font(bold(14.0)).color(Color32::WHITE))
                    .fill(DANGER)
                    .corner_radius(10.0)
                    .min_size(vec2(120.0, 36.0));
                if ui.add(del).clicked() {
                    self.actions.push(Action::Uninstall(id));
                    self.confirm_uninstall = None;
                }
                if secondary_button(ui, "Keep", TEXT_DIM, 100.0).clicked() {
                    self.confirm_uninstall = None;
                }
            });
        });
        if modal.should_close() {
            self.confirm_uninstall = None;
        }
    }

    fn toasts(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        self.toasts.retain(|t| now.duration_since(t.at) < Duration::from_secs(if t.kind == ToastKind::Error { 8 } else { 4 }));
        if self.toasts.is_empty() {
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
        egui::Area::new(Id::new("toasts"))
            .anchor(Align2::RIGHT_TOP, vec2(-18.0, 74.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                for t in self.toasts.iter().rev().take(4).rev() {
                    let color = match t.kind {
                        ToastKind::Info => ACCENT_2,
                        ToastKind::Success => SUCCESS,
                        ToastKind::Error => DANGER,
                    };
                    egui::Frame::new()
                        .fill(SURFACE)
                        .stroke(Stroke::new(1.0_f32, color.gamma_multiply(0.5)))
                        .corner_radius(10.0)
                        .inner_margin(Margin::symmetric(14, 10))
                        .shadow(egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(120) })
                        .show(ui, |ui| {
                            ui.set_max_width(380.0);
                            ui.horizontal(|ui| {
                                let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                                ui.painter().circle_filled(r.center(), 4.0, color);
                                ui.add(egui::Label::new(RichText::new(&t.text).color(TEXT).size(13.0)).wrap());
                            });
                        });
                    ui.add_space(6.0);
                }
            });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame(ctx);
    }
}

impl App {
    fn frame(&mut self, ctx: &egui::Context) {
        self.process_messages(ctx);
        self.covers.poll(ctx);

        if self.client.is_none() {
            self.login_screen(ctx);
            self.toasts(ctx);
            return;
        }

        self.update_download_stats();
        self.refresh_filter();

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && self.confirm_uninstall.is_none() && !self.show_settings {
            self.selected = None;
        }

        self.top_bar(ctx);
        self.status_bar(ctx);
        self.downloads_panel(ctx);
        self.sidebar(ctx);
        self.detail_panel(ctx);
        self.central(ctx);
        self.settings_window(ctx);
        self.confirm_dialog(ctx);
        self.toasts(ctx);

        self.apply_actions();

        if self.downloads.iter().any(|d| d.state == DlState::Running) {
            ctx.request_repaint_after(Duration::from_millis(150));
        } else if !self.downloads.is_empty() {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }
}

// ───────────────────────────── free helpers ─────────────────────────────

fn logo(ui: &mut Ui, size: f32) {
    let (r, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let p = ui.painter();
    p.rect_filled(r, size * 0.28, ACCENT);
    p.rect_filled(
        Rect::from_min_max(r.center_top(), r.right_bottom()),
        CornerRadius { nw: 0, ne: (size * 0.28) as u8, sw: 0, se: (size * 0.28) as u8 },
        ACCENT_HI.gamma_multiply(0.35),
    );
    p.text(r.center(), Align2::CENTER_CENTER, "R", bold(size * 0.56), Color32::WHITE);
}

/// Paints a cover image cropped to fill `rect`, or a placeholder.
fn paint_cover(ui: &Ui, rect: Rect, tex: Option<&TextureHandle>, radius: CornerRadius, platform: &str, large: bool) {
    match tex {
        Some(t) => {
            let [tw, th] = t.size();
            let ta = tw as f32 / th.max(1) as f32;
            let ra = rect.width() / rect.height();
            let uv = if ta > ra {
                let u = (1.0 - ra / ta) / 2.0;
                Rect::from_min_max(pos2(u, 0.0), pos2(1.0 - u, 1.0))
            } else {
                let v = (1.0 - ta / ra) / 2.0;
                Rect::from_min_max(pos2(0.0, v), pos2(1.0, 1.0 - v))
            };
            egui::Image::from_texture(t).uv(uv).corner_radius(radius).paint_at(ui, rect);
        }
        None => {
            let p = ui.painter();
            p.rect_filled(rect, radius, Color32::from_rgb(30, 34, 50));
            let label = platform.to_uppercase();
            if rect.height() > 80.0 {
                p.text(rect.center() - vec2(0.0, 10.0), Align2::CENTER_CENTER, "🎮", regular(if large { 48.0 } else { 30.0 }), BORDER_HI);
                p.text(rect.center() + vec2(0.0, 24.0), Align2::CENTER_CENTER, label, bold(11.0), MUTED);
            } else {
                p.text(rect.center(), Align2::CENTER_CENTER, "🎮", regular(16.0), BORDER_HI);
            }
        }
    }
}

fn text_field(ui: &mut Ui, label: &str, value: &mut String, hint: &str, password: bool) -> bool {
    ui.label(RichText::new(label).size(12.5).color(TEXT_DIM));
    let resp = ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .password(password)
            .desired_width(f32::INFINITY)
            .margin(Margin::symmetric(10, 7)),
    );
    resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

/// Returns true if the user pressed Enter in a field.
fn server_fields(ui: &mut Ui, cfg: &mut Config) -> bool {
    let mut submit = false;
    submit |= text_field(ui, "Server address", &mut cfg.server_url, "https://romm.example.com", false);
    ui.horizontal(|ui| {
        let half = (ui.available_width() - 8.0) / 2.0;
        ui.vertical(|ui| {
            ui.set_width(half);
            submit |= text_field(ui, "Username", &mut cfg.username, "", false);
        });
        ui.vertical(|ui| {
            ui.set_width(half);
            submit |= text_field(ui, "Password", &mut cfg.password, "", true);
        });
    });
    submit |= text_field(ui, "API token (optional, replaces username/password)", &mut cfg.api_token, "rmm_…", true);
    ui.checkbox(&mut cfg.accept_invalid_certs, "Allow self-signed HTTPS certificates");
    submit
}

fn library_field(ui: &mut Ui, dir: &mut Option<PathBuf>) {
    ui.label(RichText::new("Library folder (where installed games go)").size(12.5).color(TEXT_DIM));
    ui.horizontal(|ui| {
        let text = dir.as_ref().map(|d| d.display().to_string()).unwrap_or_else(|| "Not set".into());
        let w = ui.available_width() - 96.0;
        egui::Frame::new()
            .fill(Color32::from_rgb(10, 12, 18))
            .stroke(Stroke::new(1.0_f32, BORDER))
            .corner_radius(8.0)
            .inner_margin(Margin::symmetric(10, 7))
            .show(ui, |ui| {
                ui.set_width(w - 20.0);
                ui.add(egui::Label::new(RichText::new(&text).color(if dir.is_some() { TEXT } else { MUTED })).truncate())
                    .on_hover_text(&text);
            });
        if ui.add(egui::Button::new("Browse…").min_size(vec2(86.0, 32.0))).clicked() {
            let mut dlg = rfd::FileDialog::new().set_title("Choose your ROM library folder");
            if let Some(d) = dir.as_ref() {
                dlg = dlg.set_directory(d);
            }
            if let Some(picked) = dlg.pick_folder() {
                *dir = Some(picked);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Auth;

    fn run_frame(ctx: &egui::Context, app: &mut App, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1400.0, 900.0))),
            events,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| app.frame(ctx));
    }

    fn key(k: egui::Key) -> egui::Event {
        egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }
    }

    /// Regression: refreshing used to empty `roms` while the grid still drew
    /// stale indices in the same frame, panicking with index out of bounds.
    #[test]
    fn refresh_does_not_crash() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        apply(&ctx);
        let mut app = App::with_config(&ctx, Config::default());
        // Unreachable server: background reloads simply fail.
        app.client = Some(RommClient::new("http://127.0.0.1:9", Auth::None, false).unwrap());
        app.conn = ConnState::Connected { user: "test".into() };

        let roms: Vec<Rom> = (1..=60)
            .map(|i| Rom { id: i, name: format!("Game {i}"), platform_id: 1, fs_size_bytes: 1024, ..Default::default() })
            .collect();
        app.tx.send(Msg::RomsPage { gen: app.load_gen, roms, total: Some(60), done: true }).unwrap();

        for view in [ViewMode::Grid, ViewMode::List] {
            app.cfg.view_mode = view;
            run_frame(&ctx, &mut app, vec![]);
            run_frame(&ctx, &mut app, vec![]);
            assert_eq!(app.filtered.len(), 60, "library should be visible before refresh");
            app.selected = Some(5);

            // Press refresh (F5 shares the button's code path) and keep drawing.
            run_frame(&ctx, &mut app, vec![key(egui::Key::F5)]);
            assert!(app.roms.is_empty() && app.roms_loading, "refresh should have started");
            assert_eq!(app.selected, Some(5), "selection survives a refresh");
            run_frame(&ctx, &mut app, vec![]);

            // Repopulate for the next view mode.
            let roms: Vec<Rom> = (1..=60)
                .map(|i| Rom { id: i, name: format!("Game {i}"), platform_id: 1, ..Default::default() })
                .collect();
            app.tx.send(Msg::RomsPage { gen: app.load_gen, roms, total: Some(60), done: true }).unwrap();
        }
    }
}

#[cfg(test)]
mod detail_panel_tests {
    use super::*;
    use crate::api::Auth;

    fn frame_with(ctx: &egui::Context, app: &mut App, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1600.0, 900.0))),
            events,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| app.frame(ctx));
    }

    #[test]
    fn detail_panel_resizes_and_remembers_width() {
        let cfg_path = std::env::temp_dir().join(format!("romm-explorer-test-cfg-{}.json", std::process::id()));
        std::env::set_var("ROMM_EXPLORER_CONFIG", &cfg_path);

        let ctx = egui::Context::default();
        install_fonts(&ctx);
        apply(&ctx);
        let mut app = App::with_config(&ctx, Config::default());
        app.client = Some(RommClient::new("http://127.0.0.1:9", Auth::None, false).unwrap());
        app.conn = ConnState::Connected { user: "t".into() };
        let rom = Rom {
            id: 1,
            name: "Test".into(),
            platform_id: 1,
            metadatum: serde_json::json!({"genres": ["Role-playing (RPG)", "Turn-based strategy (TBS)", "Hack and slash/Beat 'em up", "Adventure"]}),
            regions: vec!["USA".into(), "Europe".into()],
            ..Default::default()
        };
        app.tx.send(Msg::RomsPage { gen: app.load_gen, roms: vec![rom], total: Some(1), done: true }).unwrap();
        app.selected = Some(1);

        for _ in 0..3 {
            frame_with(&ctx, &mut app, vec![]);
        }
        let start_w = app.cfg.detail_width;
        assert!((start_w - 380.0).abs() < 1.0, "default width drifted to {start_w}");

        // Drag the panel's left edge 200px to the left.
        let edge_x = 1600.0 - start_w;
        let y = 400.0;
        let ptr = |x: f32| egui::Event::PointerMoved(pos2(x, y));
        let btn = |x: f32, pressed: bool| egui::Event::PointerButton {
            pos: pos2(x, y),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        frame_with(&ctx, &mut app, vec![ptr(edge_x)]);
        frame_with(&ctx, &mut app, vec![btn(edge_x, true)]);
        for step in 1..=10 {
            frame_with(&ctx, &mut app, vec![ptr(edge_x - 20.0 * step as f32)]);
        }
        frame_with(&ctx, &mut app, vec![btn(edge_x - 200.0, false)]);
        for _ in 0..3 {
            frame_with(&ctx, &mut app, vec![]);
        }
        let new_w = app.cfg.detail_width;
        assert!((new_w - (start_w + 200.0)).abs() < 4.0, "expected ~{} after drag, got {new_w}", start_w + 200.0);

        // Saved to disk, and a fresh app opens at the same width without drifting.
        let saved: Config = serde_json::from_str(&std::fs::read_to_string(&cfg_path).unwrap()).unwrap();
        assert!((saved.detail_width - new_w).abs() < 0.5);
        let ctx2 = egui::Context::default();
        install_fonts(&ctx2);
        apply(&ctx2);
        let mut app2 = App::with_config(&ctx2, saved);
        app2.client = app.client.clone();
        app2.conn = ConnState::Connected { user: "t".into() };
        app2.tx.send(Msg::RomsPage { gen: app2.load_gen, roms: app.roms.clone(), total: Some(1), done: true }).unwrap();
        app2.selected = Some(1);
        for _ in 0..3 {
            frame_with(&ctx2, &mut app2, vec![]);
        }
        assert!((app2.cfg.detail_width - new_w).abs() < 1.0, "width drifted on reopen: {} -> {}", new_w, app2.cfg.detail_width);
        let _ = std::fs::remove_file(&cfg_path);
    }
}
