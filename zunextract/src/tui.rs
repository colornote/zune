//! TUI explorer for browsing Zune firmware contents interactively.

use crate::b000ff;
use crate::rom::{self, RomImage};
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Tabs};
use ratatui::Terminal;
use std::io::{self, Cursor, Read, Stdout};
use std::path::{Path, PathBuf};

/// A firmware entry that can be browsed
#[derive(Debug, Clone)]
enum FirmwareItem {
    /// A BIN file inside a CAB (not yet decompressed)
    CabEntry {
        name: String,
        data: Vec<u8>,
    },
    /// A parsed ROM image with its decompressed data
    RomImage {
        source_name: String,
        rom: RomImageRef,
        nb0_data: Vec<u8>,
    },
}

/// ROM image reference data (can't derive Clone for RomImage directly)
#[derive(Debug, Clone)]
struct RomImageRef {
    virtual_base: u32,
    hdr_rva: u32,
    hdr_offset: u32,
    cpu_type: u16,
    num_mods: u32,
    num_files: u32,
    phys_first: u32,
    phys_last: u32,
    kernel_flags: u32,
    modules: Vec<(String, ModuleInfo)>,
    files: Vec<(String, FileInfo)>,
}

#[derive(Debug, Clone)]
struct ModuleInfo {
    size: u32,
    load_offset: u32,
}

#[derive(Debug, Clone)]
struct FileInfo {
    real_size: u32,
    is_compressed: bool,
}

/// Current view in the TUI
#[derive(Debug, Clone, PartialEq)]
enum View {
    /// Top-level: list of BIN files (from CAB) or single BIN
    TopLevel,
    /// ROM contents: modules and files
    RomContents { item_index: usize },
}

/// Tab within ROM contents view
#[derive(Debug, Clone, Copy, PartialEq)]
enum RomTab {
    Info,
    Modules,
    Files,
}

struct App {
    items: Vec<FirmwareItem>,
    view: View,
    top_list_state: ListState,
    rom_list_state: ListState,
    rom_tab: RomTab,
    status_message: String,
    output_dir: PathBuf,
    should_quit: bool,
}

impl App {
    fn new(output_dir: PathBuf) -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            items: Vec::new(),
            view: View::TopLevel,
            top_list_state: state,
            rom_list_state: {
                let mut s = ListState::default();
                s.select(Some(0));
                s
            },
            rom_tab: RomTab::Info,
            status_message: String::new(),
            output_dir,
            should_quit: false,
        }
    }

    fn current_rom_entries_len(&self) -> usize {
        if let View::RomContents { item_index } = &self.view {
            if let Some(FirmwareItem::RomImage { rom, .. }) = self.items.get(*item_index) {
                return match self.rom_tab {
                    RomTab::Modules => rom.modules.len(),
                    RomTab::Files => rom.files.len(),
                    RomTab::Info => 0,
                };
            }
        }
        0
    }
}

fn rom_image_to_ref(rom: &RomImage) -> RomImageRef {
    RomImageRef {
        virtual_base: rom.virtual_base,
        hdr_rva: rom.hdr_rva,
        hdr_offset: rom.hdr_offset,
        cpu_type: rom.rom_hdr.cpu_type,
        num_mods: rom.rom_hdr.num_mods,
        num_files: rom.rom_hdr.num_files,
        phys_first: rom.rom_hdr.phys_first,
        phys_last: rom.rom_hdr.phys_last,
        kernel_flags: rom.rom_hdr.kernel_flags,
        modules: rom
            .modules
            .iter()
            .map(|(name, entry)| {
                (
                    name.clone(),
                    ModuleInfo {
                        size: entry.file_size,
                        load_offset: entry.load_offset,
                    },
                )
            })
            .collect(),
        files: rom
            .files
            .iter()
            .map(|(name, entry)| {
                (
                    name.clone(),
                    FileInfo {
                        real_size: entry.real_file_size,
                        is_compressed: entry.comp_file_size != entry.real_file_size,
                    },
                )
            })
            .collect(),
    }
}

/// Load firmware data from a file path
fn load_firmware(path: &Path) -> Result<Vec<FirmwareItem>> {
    let data = std::fs::read(path).with_context(|| format!("Failed to read '{}'", path.display()))?;
    let mut items = Vec::new();

    if data.len() >= 4 && &data[..4] == b"MSCF" {
        // CAB file
        let mut cabinet = cab::Cabinet::new(Cursor::new(&data[..]))
            .context("Failed to open CAB")?;
        let names: Vec<String> = cabinet
            .folder_entries()
            .flat_map(|f| f.file_entries().map(|e| e.name().to_string()))
            .collect();

        for name in names {
            let mut reader = cabinet.read_file(&name)?;
            let mut buf = Vec::new();
            reader.read_to_end(&mut buf)?;

            if b000ff::is_b000ff(&buf) {
                match b000ff::decompress(&buf) {
                    Ok((_header, nb0)) => match rom::parse(&nb0) {
                        Ok(rom_image) => {
                            let rom_ref = rom_image_to_ref(&rom_image);
                            items.push(FirmwareItem::RomImage {
                                source_name: name,
                                rom: rom_ref,
                                nb0_data: nb0,
                            });
                        }
                        Err(_) => {
                            items.push(FirmwareItem::CabEntry { name, data: buf });
                        }
                    },
                    Err(_) => {
                        items.push(FirmwareItem::CabEntry { name, data: buf });
                    }
                }
            } else {
                items.push(FirmwareItem::CabEntry { name, data: buf });
            }
        }
    } else if b000ff::is_b000ff(&data) {
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("image.bin")
            .to_string();
        let (_header, nb0) = b000ff::decompress(&data)?;
        match rom::parse(&nb0) {
            Ok(rom_image) => {
                let rom_ref = rom_image_to_ref(&rom_image);
                items.push(FirmwareItem::RomImage {
                    source_name: filename,
                    rom: rom_ref,
                    nb0_data: nb0,
                });
            }
            Err(_) => {
                items.push(FirmwareItem::CabEntry {
                    name: filename,
                    data,
                });
            }
        }
    } else {
        // Try as raw ROM image
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("image")
            .to_string();
        match rom::parse(&data) {
            Ok(rom_image) => {
                let rom_ref = rom_image_to_ref(&rom_image);
                items.push(FirmwareItem::RomImage {
                    source_name: filename,
                    rom: rom_ref,
                    nb0_data: data,
                });
            }
            Err(_) => {
                items.push(FirmwareItem::CabEntry {
                    name: filename,
                    data,
                });
            }
        }
    }

    Ok(items)
}

fn cpu_name(cpu_type: u16) -> &'static str {
    match cpu_type {
        0x01c0 => "ARM",
        0x01c2 => "ARM Thumb",
        0x01c4 => "ARMv7",
        0x0166 => "MIPS R4000",
        0x01f0 => "PowerPC",
        0x014c => "x86",
        _ => "Unknown",
    }
}

fn format_size(bytes: u32) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

fn draw(f: &mut ratatui::Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Title
            Constraint::Min(0),   // Content
            Constraint::Length(3), // Status/help
        ])
        .split(f.area());

    // Title bar
    let title = Paragraph::new(Line::from(vec![
        Span::styled(" zunextract ", Style::default().fg(Color::Black).bg(Color::Green).add_modifier(Modifier::BOLD)),
        Span::raw("  Zune HD Firmware Explorer"),
    ]))
    .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, chunks[0]);

    match &app.view {
        View::TopLevel => draw_top_level(f, app, chunks[1]),
        View::RomContents { .. } => draw_rom_contents(f, app, chunks[1]),
    }

    // Status bar
    let help_text = match &app.view {
        View::TopLevel => "↑↓ Navigate | Enter: Open | e: Extract All | q: Quit",
        View::RomContents { .. } => "←→ Tab | ↑↓ Navigate | e: Extract | Esc: Back | q: Quit",
    };
    let status = Paragraph::new(Line::from(vec![
        Span::styled(&app.status_message, Style::default().fg(Color::Yellow)),
        Span::raw("  "),
        Span::styled(help_text, Style::default().fg(Color::DarkGray)),
    ]))
    .block(Block::default().borders(Borders::ALL));
    f.render_widget(status, chunks[2]);
}

fn draw_top_level(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = app
        .items
        .iter()
        .map(|item| {
            let (name, detail) = match item {
                FirmwareItem::CabEntry { name, data } => {
                    (name.clone(), format!("{} (raw)", format_size(data.len() as u32)))
                }
                FirmwareItem::RomImage { source_name, rom, .. } => {
                    (
                        source_name.clone(),
                        format!(
                            "{} modules, {} files | {}",
                            rom.num_mods,
                            rom.num_files,
                            cpu_name(rom.cpu_type)
                        ),
                    )
                }
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {:<30}", name),
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                ),
                Span::styled(detail, Style::default().fg(Color::Cyan)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Firmware Images ")
                .borders(Borders::ALL),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    f.render_stateful_widget(list, area, &mut app.top_list_state);
}

fn draw_rom_contents(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let item_index = if let View::RomContents { item_index } = &app.view {
        *item_index
    } else {
        return;
    };

    let (source_name, rom) = match &app.items[item_index] {
        FirmwareItem::RomImage {
            source_name, rom, ..
        } => (source_name.clone(), rom.clone()),
        _ => return,
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    // Tabs
    let tab_titles = vec!["Info", "Modules", "Files"];
    let selected_tab = match app.rom_tab {
        RomTab::Info => 0,
        RomTab::Modules => 1,
        RomTab::Files => 2,
    };
    let tabs = Tabs::new(tab_titles)
        .block(
            Block::default()
                .title(format!(" {} ", source_name))
                .borders(Borders::ALL),
        )
        .select(selected_tab)
        .style(Style::default().fg(Color::White))
        .highlight_style(
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, chunks[0]);

    match app.rom_tab {
        RomTab::Info => {
            let info_text = vec![
                Line::from(vec![
                    Span::styled("  CPU:           ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("{} ({:#06x})", cpu_name(rom.cpu_type), rom.cpu_type),
                        Style::default().fg(Color::White),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("  Virtual base:  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{:#010x}", rom.virtual_base), Style::default().fg(Color::Cyan)),
                ]),
                Line::from(vec![
                    Span::styled("  ROMHDR RVA:    ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{:#010x}", rom.hdr_rva), Style::default().fg(Color::Cyan)),
                ]),
                Line::from(vec![
                    Span::styled("  ROMHDR offset: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{:#010x}", rom.hdr_offset), Style::default().fg(Color::Cyan)),
                ]),
                Line::from(vec![
                    Span::styled("  Phys first:    ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{:#010x}", rom.phys_first), Style::default().fg(Color::Cyan)),
                ]),
                Line::from(vec![
                    Span::styled("  Phys last:     ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{:#010x}", rom.phys_last), Style::default().fg(Color::Cyan)),
                ]),
                Line::from(vec![
                    Span::styled("  Kernel flags:  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{:#010x}", rom.kernel_flags), Style::default().fg(Color::Cyan)),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  Modules:       ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{}", rom.num_mods), Style::default().fg(Color::Green)),
                ]),
                Line::from(vec![
                    Span::styled("  Files:         ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{}", rom.num_files), Style::default().fg(Color::Green)),
                ]),
                Line::from(vec![
                    Span::styled("  Image size:    ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format_size(rom.phys_last.wrapping_sub(rom.phys_first)),
                        Style::default().fg(Color::Green),
                    ),
                ]),
            ];
            let info = Paragraph::new(info_text)
                .block(Block::default().title(" ROM Info ").borders(Borders::ALL));
            f.render_widget(info, chunks[1]);
        }
        RomTab::Modules => {
            let items: Vec<ListItem> = rom
                .modules
                .iter()
                .map(|(name, info)| {
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            format!(" {:<40}", name),
                            Style::default().fg(Color::White),
                        ),
                        Span::styled(
                            format!("{:>10}", format_size(info.size)),
                            Style::default().fg(Color::Cyan),
                        ),
                        Span::styled(
                            format!("  load={:#010x}", info.load_offset),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]))
                })
                .collect();

            let list = List::new(items)
                .block(
                    Block::default()
                        .title(format!(" Modules ({}) ", rom.num_mods))
                        .borders(Borders::ALL),
                )
                .highlight_style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("▶ ");

            f.render_stateful_widget(list, chunks[1], &mut app.rom_list_state);
        }
        RomTab::Files => {
            let items: Vec<ListItem> = rom
                .files
                .iter()
                .map(|(name, info)| {
                    let comp_indicator = if info.is_compressed {
                        Span::styled(" [compressed]", Style::default().fg(Color::Yellow))
                    } else {
                        Span::raw("")
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            format!(" {:<40}", name),
                            Style::default().fg(Color::White),
                        ),
                        Span::styled(
                            format!("{:>10}", format_size(info.real_size)),
                            Style::default().fg(Color::Cyan),
                        ),
                        comp_indicator,
                    ]))
                })
                .collect();

            let list = List::new(items)
                .block(
                    Block::default()
                        .title(format!(" Files ({}) ", rom.num_files))
                        .borders(Borders::ALL),
                )
                .highlight_style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("▶ ");

            f.render_stateful_widget(list, chunks[1], &mut app.rom_list_state);
        }
    }
}

fn handle_key(app: &mut App, key: KeyCode, modifiers: KeyModifiers) {
    if key == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return;
    }

    match &app.view {
        View::TopLevel => match key {
            KeyCode::Char('q') => app.should_quit = true,
            KeyCode::Up => {
                let i = app.top_list_state.selected().unwrap_or(0);
                if i > 0 {
                    app.top_list_state.select(Some(i - 1));
                }
            }
            KeyCode::Down => {
                let i = app.top_list_state.selected().unwrap_or(0);
                if i + 1 < app.items.len() {
                    app.top_list_state.select(Some(i + 1));
                }
            }
            KeyCode::Enter => {
                if let Some(i) = app.top_list_state.selected() {
                    if matches!(app.items.get(i), Some(FirmwareItem::RomImage { .. })) {
                        app.view = View::RomContents { item_index: i };
                        app.rom_tab = RomTab::Info;
                        app.rom_list_state.select(Some(0));
                        app.status_message.clear();
                    } else {
                        app.status_message = "Raw entry - no ROM data to explore".to_string();
                    }
                }
            }
            KeyCode::Char('e') => {
                // Extract all
                app.status_message = "Extracting all...".to_string();
                let dir = app.output_dir.clone();
                let mut ok_count = 0;
                let mut err_count = 0;
                for item in &app.items {
                    if let FirmwareItem::RomImage {
                        source_name,
                        nb0_data,
                        ..
                    } = item
                    {
                        let stem = Path::new(source_name)
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or(source_name);
                        match rom::parse(nb0_data) {
                            Ok(rom_image) => {
                                let rom_dir = dir.join(format!("{}_rom", stem));
                                let _ = std::fs::create_dir_all(&rom_dir);
                                match rom::extract(nb0_data, &rom_image, &rom_dir) {
                                    Ok(_) => ok_count += 1,
                                    Err(_) => err_count += 1,
                                }
                            }
                            Err(_) => err_count += 1,
                        }
                    }
                }
                app.status_message = format!(
                    "Extracted {} images ({} errors) to {}",
                    ok_count,
                    err_count,
                    dir.display()
                );
            }
            _ => {}
        },
        View::RomContents { item_index } => {
            let item_index = *item_index;
            match key {
                KeyCode::Char('q') => app.should_quit = true,
                KeyCode::Esc => {
                    app.view = View::TopLevel;
                    app.status_message.clear();
                }
                KeyCode::Left => {
                    app.rom_tab = match app.rom_tab {
                        RomTab::Info => RomTab::Files,
                        RomTab::Modules => RomTab::Info,
                        RomTab::Files => RomTab::Modules,
                    };
                    app.rom_list_state.select(Some(0));
                }
                KeyCode::Right | KeyCode::Tab => {
                    app.rom_tab = match app.rom_tab {
                        RomTab::Info => RomTab::Modules,
                        RomTab::Modules => RomTab::Files,
                        RomTab::Files => RomTab::Info,
                    };
                    app.rom_list_state.select(Some(0));
                }
                KeyCode::Up => {
                    let i = app.rom_list_state.selected().unwrap_or(0);
                    if i > 0 {
                        app.rom_list_state.select(Some(i - 1));
                    }
                }
                KeyCode::Down => {
                    let i = app.rom_list_state.selected().unwrap_or(0);
                    let max = app.current_rom_entries_len();
                    if max > 0 && i + 1 < max {
                        app.rom_list_state.select(Some(i + 1));
                    }
                }
                KeyCode::Char('e') => {
                    // Extract this ROM image
                    if let Some(FirmwareItem::RomImage {
                        source_name,
                        nb0_data,
                        ..
                    }) = app.items.get(item_index)
                    {
                        let stem = Path::new(source_name)
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or(source_name);
                        let dir = app.output_dir.join(format!("{}_rom", stem));
                        let _ = std::fs::create_dir_all(&dir);
                        match rom::parse(nb0_data) {
                            Ok(rom_image) => match rom::extract(nb0_data, &rom_image, &dir) {
                                Ok(_) => {
                                    app.status_message = format!(
                                        "Extracted {} modules + {} files to {}",
                                        rom_image.modules.len(),
                                        rom_image.files.len(),
                                        dir.display()
                                    );
                                }
                                Err(e) => {
                                    app.status_message = format!("Extract error: {}", e);
                                }
                            },
                            Err(e) => {
                                app.status_message = format!("Parse error: {}", e);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

pub fn run(path: &Path, output_dir: &Path) -> Result<()> {
    // Load firmware
    eprintln!("Loading firmware from {}...", path.display());
    let items = load_firmware(path)?;
    if items.is_empty() {
        return Err(anyhow::anyhow!("No firmware images found"));
    }
    eprintln!("Loaded {} entries", items.len());

    // Set up terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(output_dir.to_path_buf());
    app.items = items;

    let result = run_app(&mut terminal, &mut app);

    // Restore terminal
    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    result
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| draw(f, app))?;

        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press {
                handle_key(app, key.code, key.modifiers);
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}
