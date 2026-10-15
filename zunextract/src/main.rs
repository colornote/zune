mod b000ff;
mod cecompress;
mod rom;
mod tui;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "zunextract",
    version,
    about = "Extract files from Zune HD (Windows CE 6.0) firmware CAB and BIN files"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show information about a firmware file (CAB, BIN, or NB0)
    Info {
        /// Input file path
        input: PathBuf,
    },

    /// Extract all contents from a firmware file
    Extract {
        /// Input file path (CAB, BIN, or NB0)
        input: PathBuf,

        /// Output directory
        #[arg(short, long, default_value = "output")]
        output: PathBuf,
    },

    /// Extract BIN files from a CAB archive only
    Cab {
        /// Input CAB file
        input: PathBuf,

        /// Output directory
        #[arg(short, long, default_value = "output")]
        output: PathBuf,
    },

    /// Decompress a B000FF BIN file to raw NB0 format
    Bin {
        /// Input BIN file
        input: PathBuf,

        /// Output NB0 file (default: <input>.nb0)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Extract files from a decompressed NB0 ROM image
    Rom {
        /// Input NB0 file (decompressed)
        input: PathBuf,

        /// Output directory
        #[arg(short, long, default_value = "output")]
        output: PathBuf,
    },

    /// Interactive TUI explorer for browsing firmware contents
    Explore {
        /// Input file path (CAB, BIN, or NB0)
        input: PathBuf,

        /// Output directory for extraction
        #[arg(short, long, default_value = "output")]
        output: PathBuf,
    },
}

/// Detect file type from content
enum FileType {
    Cab,
    B000ff,
    Raw,
}

fn detect_file_type(data: &[u8]) -> FileType {
    if data.len() >= 4 && &data[..4] == b"MSCF" {
        FileType::Cab
    } else if b000ff::is_b000ff(data) {
        FileType::B000ff
    } else {
        FileType::Raw
    }
}

/// Extract files from a CAB archive, returning list of (filename, data) pairs
fn extract_cab(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut cabinet = cab::Cabinet::new(Cursor::new(data))
        .context("Failed to open CAB archive")?;

    let names: Vec<String> = cabinet
        .folder_entries()
        .flat_map(|f| f.file_entries().map(|e| e.name().to_string()))
        .collect();

    let mut files = Vec::new();
    for name in names {
        let mut reader = cabinet.read_file(&name)
            .with_context(|| format!("Failed to read '{}' from CAB", name))?;
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;
        files.push((name, buf));
    }

    Ok(files)
}

/// Process a single BIN file: decompress B000FF and extract ROM contents
fn process_bin(
    name: &str,
    data: &[u8],
    output_dir: &Path,
    extract_rom: bool,
) -> Result<()> {
    let (_header, decompressed) = b000ff::decompress(data)
        .with_context(|| format!("Failed to decompress '{}'", name))?;

    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);

    // Save the decompressed NB0
    let nb0_path = output_dir.join(format!("{}.nb0", stem));
    std::fs::write(&nb0_path, &decompressed)
        .with_context(|| format!("Failed to write '{}'", nb0_path.display()))?;
    println!("Wrote {} ({} bytes)", nb0_path.display(), decompressed.len());

    if extract_rom {
        // Try to parse as ROM image
        match rom::parse(&decompressed) {
            Ok(rom_image) => {
                println!();
                rom::print_info(&rom_image);
                println!();

                let rom_dir = output_dir.join(format!("{}_rom", stem));
                std::fs::create_dir_all(&rom_dir)?;

                println!("Extracting to {}:", rom_dir.display());
                rom::extract(&decompressed, &rom_image, &rom_dir)?;
            }
            Err(e) => {
                eprintln!(
                    "Note: '{}' does not contain a standard XIP ROM image: {}",
                    name, e
                );
                eprintln!("  The NB0 file was still saved for manual analysis.");
            }
        }
    }

    Ok(())
}

fn cmd_info(input: &Path) -> Result<()> {
    let data = std::fs::read(input)
        .with_context(|| format!("Failed to read '{}'", input.display()))?;

    match detect_file_type(&data) {
        FileType::Cab => {
            println!("File type: Microsoft CAB archive");
            let files = extract_cab(&data)?;
            println!("Contents:");
            for (name, data) in &files {
                let type_str = if b000ff::is_b000ff(data) {
                    let hdr = b000ff::parse_header(data)?;
                    format!(
                        "B000FF (start={:#x}, length={:#x})",
                        hdr.image_start, hdr.image_length
                    )
                } else {
                    "Unknown".to_string()
                };
                println!("  {:30} {:>10} bytes  [{}]", name, data.len(), type_str);
            }
        }
        FileType::B000ff => {
            println!("File type: B000FF Windows CE firmware image");
            let hdr = b000ff::parse_header(&data)?;
            println!("  Image start:  {:#010x}", hdr.image_start);
            println!("  Image length: {:#010x} ({} bytes)", hdr.image_length, hdr.image_length);

            println!("\nDecompressing to parse ROM...");
            let (_, decompressed) = b000ff::decompress(&data)?;

            match rom::parse(&decompressed) {
                Ok(rom_image) => {
                    println!();
                    rom::print_info(&rom_image);
                }
                Err(e) => {
                    eprintln!("\nNo standard XIP ROM image found: {}", e);
                }
            }
        }
        FileType::Raw => {
            println!("File type: Raw binary image");
            println!("  Size: {} bytes ({:#x})", data.len(), data.len());

            match rom::parse(&data) {
                Ok(rom_image) => {
                    println!();
                    rom::print_info(&rom_image);
                }
                Err(e) => {
                    eprintln!("No XIP ROM image found: {}", e);
                }
            }
        }
    }

    Ok(())
}

fn cmd_extract(input: &Path, output: &Path) -> Result<()> {
    let data = std::fs::read(input)
        .with_context(|| format!("Failed to read '{}'", input.display()))?;

    std::fs::create_dir_all(output)?;

    match detect_file_type(&data) {
        FileType::Cab => {
            println!("Extracting CAB archive: {}", input.display());
            let files = extract_cab(&data)?;

            for (name, file_data) in &files {
                // Save raw BIN file
                let bin_path = output.join(name);
                std::fs::write(&bin_path, file_data)?;
                println!("Extracted {} ({} bytes)", name, file_data.len());

                // If it's a B000FF file, decompress and extract
                if b000ff::is_b000ff(file_data) {
                    process_bin(name, file_data, output, true)?;
                }
            }
        }
        FileType::B000ff => {
            println!("Processing B000FF image: {}", input.display());
            let name = input
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("image.bin");
            process_bin(name, &data, output, true)?;
        }
        FileType::Raw => {
            println!("Processing raw image: {}", input.display());
            match rom::parse(&data) {
                Ok(rom_image) => {
                    rom::print_info(&rom_image);
                    println!();
                    println!("Extracting:");
                    rom::extract(&data, &rom_image, output)?;
                }
                Err(e) => {
                    return Err(anyhow!(
                        "File is not a recognized format (not CAB, B000FF, or XIP ROM): {}",
                        e
                    ));
                }
            }
        }
    }

    Ok(())
}

fn cmd_cab(input: &Path, output: &Path) -> Result<()> {
    let data = std::fs::read(input)
        .with_context(|| format!("Failed to read '{}'", input.display()))?;

    if &data[..4] != b"MSCF" {
        return Err(anyhow!("Not a CAB file"));
    }

    std::fs::create_dir_all(output)?;

    let files = extract_cab(&data)?;
    for (name, file_data) in &files {
        let out_path = output.join(name);
        std::fs::write(&out_path, file_data)?;
        println!("{} ({} bytes)", name, file_data.len());
    }

    Ok(())
}

fn cmd_bin(input: &Path, output: Option<&Path>) -> Result<()> {
    let data = std::fs::read(input)
        .with_context(|| format!("Failed to read '{}'", input.display()))?;

    if !b000ff::is_b000ff(&data) {
        return Err(anyhow!("Not a B000FF file"));
    }

    let default_output = input.with_extension("nb0");
    let output = output.unwrap_or(&default_output);

    let (header, decompressed) = b000ff::decompress(&data)?;
    std::fs::write(output, &decompressed)?;
    println!(
        "Decompressed to {} ({} bytes, image_start={:#x})",
        output.display(),
        decompressed.len(),
        header.image_start
    );

    Ok(())
}

fn cmd_rom(input: &Path, output: &Path) -> Result<()> {
    let data = std::fs::read(input)
        .with_context(|| format!("Failed to read '{}'", input.display()))?;

    std::fs::create_dir_all(output)?;

    let rom_image = rom::parse(&data)
        .context("Failed to parse ROM image")?;

    rom::print_info(&rom_image);
    println!();
    println!("Extracting to {}:", output.display());
    rom::extract(&data, &rom_image, output)?;

    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Info { input } => cmd_info(input),
        Commands::Extract { input, output } => cmd_extract(input, output),
        Commands::Cab { input, output } => cmd_cab(input, output),
        Commands::Bin { input, output } => cmd_bin(input, output.as_deref()),
        Commands::Rom { input, output } => cmd_rom(input, output),
        Commands::Explore { input, output } => tui::run(input, output),
    }
}
