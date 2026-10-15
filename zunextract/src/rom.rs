//! Windows CE ROM image parser.
//!
//! Parses XIP (eXecute-In-Place) regions from decompressed CE images,
//! extracting modules (DLLs/EXEs) and files.
//!
//! The ROM format uses:
//! - ECEC signature at offset 0x40 pointing to ROMHDR
//! - ROMHDR containing counts of modules and files
//! - TOC entries for modules (with e32_rom/o32_rom PE-like structures)
//! - File entries for data files

use crate::cecompress;
use anyhow::{anyhow, Context, Result};
use std::path::Path;

const ECEC_SIGNATURE: u32 = 0x43454345;

/// ROM header structure (84 bytes)
#[derive(Debug)]
pub struct RomHdr {
    pub phys_first: u32,
    pub phys_last: u32,
    pub num_mods: u32,
    pub num_files: u32,
    pub kernel_flags: u32,
    pub cpu_type: u16,
}

/// TOC entry for a module (32 bytes)
#[derive(Debug)]
pub struct TocEntry {
    pub file_attributes: u32,
    pub file_size: u32,
    pub file_name_addr: u32,
    pub e32_offset: u32,
    pub o32_offset: u32,
    pub load_offset: u32,
}

/// File entry (28 bytes)
#[derive(Debug)]
pub struct FileEntry {
    pub file_attributes: u32,
    pub real_file_size: u32,
    pub comp_file_size: u32,
    pub file_name_addr: u32,
    pub load_offset: u32,
}

/// e32_rom structure - PE header equivalent in ROM (112 bytes for CE 6.0)
#[derive(Debug)]
pub struct E32Rom {
    pub obj_cnt: u16,
    pub image_flags: u16,
    pub entry_rva: u32,
    pub vbase: u32,
    pub stack_max: u32,
    pub vsize: u32,
    pub timestamp: u32,
    // info[9] - 9 data directory entries (rva, size pairs) = 72 bytes
    pub data_dirs: Vec<(u32, u32)>,
    pub subsys: u16,
}

/// o32_rom structure - Section header equivalent in ROM (24 bytes)
#[derive(Debug)]
pub struct O32Rom {
    pub vsize: u32,
    pub rva: u32,
    pub psize: u32,
    pub data_ptr: u32,
    pub flags: u32,
}

/// Parsed ROM image with all extracted metadata
#[derive(Debug)]
pub struct RomImage {
    pub hdr_rva: u32,
    pub hdr_offset: u32,
    pub virtual_base: u32,
    pub rom_hdr: RomHdr,
    pub modules: Vec<(String, TocEntry)>,
    pub files: Vec<(String, FileEntry)>,
}

const IMAGE_SCN_COMPRESSED: u32 = 0x2000;

fn read_u16(data: &[u8], offset: usize) -> Result<u16> {
    if offset + 2 > data.len() {
        return Err(anyhow!("Read u16 out of bounds at offset {:#x}", offset));
    }
    Ok(u16::from_le_bytes(data[offset..offset + 2].try_into()?))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32> {
    if offset + 4 > data.len() {
        return Err(anyhow!("Read u32 out of bounds at offset {:#x}", offset));
    }
    Ok(u32::from_le_bytes(data[offset..offset + 4].try_into()?))
}

/// Read a null-terminated ASCII string from the image
fn read_cstring(data: &[u8], offset: usize) -> Result<String> {
    if offset >= data.len() {
        return Err(anyhow!("String offset {:#x} out of bounds", offset));
    }
    let end = data[offset..]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(data.len() - offset)
        .min(260); // MAX_PATH
    Ok(String::from_utf8_lossy(&data[offset..offset + end]).to_string())
}

/// Convert an address (RVA) to a file offset within the decompressed image
fn addr_to_offset(addr: u32, virtual_base: u32) -> Result<usize> {
    addr.checked_sub(virtual_base)
        .map(|v| v as usize)
        .ok_or_else(|| anyhow!("Address {:#x} below virtual base {:#x}", addr, virtual_base))
}

/// Find ECEC signature in the image and return (hdr_rva, hdr_offset)
fn find_ecec(data: &[u8]) -> Result<(u32, u32)> {
    // First check the standard location at offset 0x40
    if data.len() > 0x4C {
        let sig = read_u32(data, 0x40)?;
        if sig == ECEC_SIGNATURE {
            let hdr_rva = read_u32(data, 0x44)?;
            let hdr_offset = read_u32(data, 0x48)?;
            return Ok((hdr_rva, hdr_offset));
        }
    }

    // Scan for ECEC at 4-byte aligned positions
    // In WinCE, the ECEC signature is typically part of a pTOC structure
    // where ECEC is followed by RVA and file offset of ROMHDR
    for offset in (0..data.len().saturating_sub(12)).step_by(4) {
        if let Ok(val) = read_u32(data, offset) {
            if val == ECEC_SIGNATURE {
                let hdr_rva = read_u32(data, offset + 4)?;
                let hdr_offset = read_u32(data, offset + 8)?;
                // Validate: hdr_offset should point within the image
                if (hdr_offset as usize) < data.len() {
                    eprintln!(
                        "Found ECEC at offset {:#x} (non-standard location)",
                        offset
                    );
                    return Ok((hdr_rva, hdr_offset));
                }
            }
        }
    }

    Err(anyhow!("ECEC signature not found in image"))
}

/// Parse the ROMHDR structure at the given offset
fn parse_romhdr(data: &[u8], offset: usize) -> Result<RomHdr> {
    if offset + 84 > data.len() {
        return Err(anyhow!(
            "ROMHDR extends beyond image (offset={:#x}, image_len={:#x})",
            offset,
            data.len()
        ));
    }

    Ok(RomHdr {
        phys_first: read_u32(data, offset + 8)?,
        phys_last: read_u32(data, offset + 12)?,
        num_mods: read_u32(data, offset + 16)?,
        num_files: read_u32(data, offset + 48)?,
        kernel_flags: read_u32(data, offset + 52)?,
        cpu_type: read_u16(data, offset + 68)?,
    })
}

/// Parse a TOC entry (module) at the given offset
fn parse_toc_entry(data: &[u8], offset: usize) -> Result<TocEntry> {
    Ok(TocEntry {
        file_attributes: read_u32(data, offset)?,
        file_size: read_u32(data, offset + 12)?,
        file_name_addr: read_u32(data, offset + 16)?,
        e32_offset: read_u32(data, offset + 20)?,
        o32_offset: read_u32(data, offset + 24)?,
        load_offset: read_u32(data, offset + 28)?,
    })
}

/// Parse a file entry at the given offset
fn parse_file_entry(data: &[u8], offset: usize) -> Result<FileEntry> {
    Ok(FileEntry {
        file_attributes: read_u32(data, offset)?,
        real_file_size: read_u32(data, offset + 12)?,
        comp_file_size: read_u32(data, offset + 16)?,
        file_name_addr: read_u32(data, offset + 20)?,
        load_offset: read_u32(data, offset + 24)?,
    })
}

/// Parse e32_rom structure
fn parse_e32_rom(data: &[u8], offset: usize) -> Result<E32Rom> {
    if offset + 112 > data.len() {
        return Err(anyhow!("e32_rom extends beyond image at offset {:#x}", offset));
    }

    let obj_cnt = read_u16(data, offset)?;
    let image_flags = read_u16(data, offset + 2)?;
    let entry_rva = read_u32(data, offset + 4)?;
    let vbase = read_u32(data, offset + 8)?;
    let stack_max = read_u32(data, offset + 16)?;
    let vsize = read_u32(data, offset + 20)?;
    let timestamp = read_u32(data, offset + 32)?;

    // 9 data directory entries at offset+36, each 8 bytes (rva, size)
    let mut data_dirs = Vec::new();
    for i in 0..9 {
        let dd_offset = offset + 36 + i * 8;
        let rva = read_u32(data, dd_offset)?;
        let size = read_u32(data, dd_offset + 4)?;
        data_dirs.push((rva, size));
    }

    let subsys = read_u16(data, offset + 108)?;

    Ok(E32Rom {
        obj_cnt,
        image_flags,
        entry_rva,
        vbase,
        stack_max,
        vsize,
        timestamp,
        data_dirs,
        subsys,
    })
}

/// Parse o32_rom structure
fn parse_o32_rom(data: &[u8], offset: usize) -> Result<O32Rom> {
    if offset + 24 > data.len() {
        return Err(anyhow!("o32_rom extends beyond image at offset {:#x}", offset));
    }

    Ok(O32Rom {
        vsize: read_u32(data, offset)?,
        rva: read_u32(data, offset + 4)?,
        psize: read_u32(data, offset + 8)?,
        data_ptr: read_u32(data, offset + 12)?,
        flags: read_u32(data, offset + 20)?,
    })
}

/// Parse a complete ROM image
pub fn parse(data: &[u8]) -> Result<RomImage> {
    let (hdr_rva, hdr_offset) = find_ecec(data).context("Failed to find ECEC signature")?;
    let virtual_base = hdr_rva.wrapping_sub(hdr_offset);

    eprintln!(
        "ECEC found: hdr_rva={:#x}, hdr_offset={:#x}, virtual_base={:#x}",
        hdr_rva, hdr_offset, virtual_base
    );

    let rom_hdr = parse_romhdr(data, hdr_offset as usize)
        .context("Failed to parse ROMHDR")?;

    eprintln!(
        "ROMHDR: {} modules, {} files, cpu_type={:#x}",
        rom_hdr.num_mods, rom_hdr.num_files, rom_hdr.cpu_type
    );
    eprintln!(
        "  phys_first={:#x}, phys_last={:#x}",
        rom_hdr.phys_first, rom_hdr.phys_last
    );

    // TOC entries start right after ROMHDR (84 bytes)
    let toc_start = hdr_offset as usize + 84;
    let mut modules = Vec::new();

    for i in 0..rom_hdr.num_mods {
        let entry_offset = toc_start + (i as usize) * 32;
        match parse_toc_entry(data, entry_offset) {
            Ok(entry) => {
                let name = match addr_to_offset(entry.file_name_addr, virtual_base) {
                    Ok(name_offset) => read_cstring(data, name_offset)
                        .unwrap_or_else(|_| format!("module_{}", i)),
                    Err(_) => format!("module_{}", i),
                };
                modules.push((name, entry));
            }
            Err(e) => {
                eprintln!("Warning: failed to parse module {}: {}", i, e);
            }
        }
    }

    // File entries start after all TOC entries
    let files_start = toc_start + (rom_hdr.num_mods as usize) * 32;
    let mut files = Vec::new();

    for i in 0..rom_hdr.num_files {
        let entry_offset = files_start + (i as usize) * 28;
        match parse_file_entry(data, entry_offset) {
            Ok(entry) => {
                let name = match addr_to_offset(entry.file_name_addr, virtual_base) {
                    Ok(name_offset) => read_cstring(data, name_offset)
                        .unwrap_or_else(|_| format!("file_{}", i)),
                    Err(_) => format!("file_{}", i),
                };
                files.push((name, entry));
            }
            Err(e) => {
                eprintln!("Warning: failed to parse file {}: {}", i, e);
            }
        }
    }

    Ok(RomImage {
        hdr_rva,
        hdr_offset,
        virtual_base,
        rom_hdr,
        modules,
        files,
    })
}

/// Build a PE file from a module's e32_rom/o32_rom data
fn build_pe(
    data: &[u8],
    e32: &E32Rom,
    sections: &[O32Rom],
    virtual_base: u32,
    cpu_type: u16,
) -> Result<Vec<u8>> {
    // PE header layout:
    // DOS header (64 bytes) + PE signature (4) + COFF header (20) + Optional header (224)
    // + Section headers (40 * n)
    let dos_header_size = 64;
    let pe_sig_size = 4;
    let coff_header_size = 20;
    let optional_header_size = 224; // PE32 optional header
    let section_header_size = 40;
    let headers_size = dos_header_size
        + pe_sig_size
        + coff_header_size
        + optional_header_size
        + section_header_size * sections.len();
    // Align to 512 bytes (file alignment)
    let file_alignment = 0x200u32;
    let aligned_headers = ((headers_size as u32 + file_alignment - 1) / file_alignment) * file_alignment;

    let mut pe = vec![0u8; aligned_headers as usize];

    // DOS header
    pe[0] = b'M';
    pe[1] = b'Z';
    // e_lfanew at offset 0x3C - points to PE signature
    let pe_offset = dos_header_size as u32;
    pe[0x3C..0x40].copy_from_slice(&pe_offset.to_le_bytes());

    // PE signature
    let sig_off = dos_header_size;
    pe[sig_off] = b'P';
    pe[sig_off + 1] = b'E';

    // COFF header
    let coff_off = sig_off + pe_sig_size;
    // Machine type
    let machine = match cpu_type {
        0x01c0 | 0x01c2 | 0x01c4 => 0x01c0u16, // ARM
        0x0166 => 0x0166u16,                      // MIPS R4000
        0x01f0 => 0x01f0u16,                      // PowerPC
        0x014c => 0x014cu16,                       // x86
        _ => cpu_type,
    };
    pe[coff_off..coff_off + 2].copy_from_slice(&machine.to_le_bytes());
    // NumberOfSections
    pe[coff_off + 2..coff_off + 4].copy_from_slice(&(sections.len() as u16).to_le_bytes());
    // TimeDateStamp
    pe[coff_off + 4..coff_off + 8].copy_from_slice(&e32.timestamp.to_le_bytes());
    // SizeOfOptionalHeader
    pe[coff_off + 16..coff_off + 18].copy_from_slice(&(optional_header_size as u16).to_le_bytes());
    // Characteristics: EXECUTABLE_IMAGE | 32BIT_MACHINE | DLL (if applicable)
    let mut characteristics: u16 = 0x0102; // EXECUTABLE_IMAGE | 32BIT_MACHINE
    if e32.image_flags & 0x0002 != 0 {
        characteristics |= 0x2000; // DLL
    }
    pe[coff_off + 18..coff_off + 20].copy_from_slice(&characteristics.to_le_bytes());

    // Optional header
    let opt_off = coff_off + coff_header_size;
    // Magic (PE32)
    pe[opt_off..opt_off + 2].copy_from_slice(&0x010bu16.to_le_bytes());
    // AddressOfEntryPoint
    pe[opt_off + 16..opt_off + 20].copy_from_slice(&e32.entry_rva.to_le_bytes());
    // ImageBase
    pe[opt_off + 28..opt_off + 32].copy_from_slice(&e32.vbase.to_le_bytes());
    // SectionAlignment (typically 0x1000)
    pe[opt_off + 32..opt_off + 36].copy_from_slice(&0x1000u32.to_le_bytes());
    // FileAlignment
    pe[opt_off + 36..opt_off + 40].copy_from_slice(&file_alignment.to_le_bytes());
    // SizeOfImage
    pe[opt_off + 56..opt_off + 60].copy_from_slice(&e32.vsize.to_le_bytes());
    // SizeOfHeaders
    pe[opt_off + 60..opt_off + 64].copy_from_slice(&aligned_headers.to_le_bytes());
    // Subsystem
    pe[opt_off + 68..opt_off + 70].copy_from_slice(&e32.subsys.to_le_bytes());
    // SizeOfStackReserve
    pe[opt_off + 72..opt_off + 76].copy_from_slice(&e32.stack_max.to_le_bytes());
    // NumberOfRvaAndSizes
    pe[opt_off + 92..opt_off + 96].copy_from_slice(&9u32.to_le_bytes());
    // Data directories
    for (i, &(rva, size)) in e32.data_dirs.iter().enumerate() {
        let dd_off = opt_off + 96 + i * 8;
        pe[dd_off..dd_off + 4].copy_from_slice(&rva.to_le_bytes());
        pe[dd_off + 4..dd_off + 8].copy_from_slice(&size.to_le_bytes());
    }

    // Section headers and data
    let sect_hdr_off = opt_off + optional_header_size;
    let mut file_offset = aligned_headers;

    // Section name mapping based on flags
    fn section_name(flags: u32, index: usize) -> [u8; 8] {
        let name = if flags & 0x00000020 != 0 {
            ".text"
        } else if flags & 0x00000040 != 0 {
            ".idata"
        } else if flags & 0x00000080 != 0 {
            ".data"
        } else if flags & 0x02000000 != 0 {
            ".pdata"
        } else {
            match index {
                0 => ".text",
                1 => ".data",
                _ => ".rsrc",
            }
        };
        let mut buf = [0u8; 8];
        buf[..name.len().min(8)].copy_from_slice(&name.as_bytes()[..name.len().min(8)]);
        buf
    }

    for (i, sect) in sections.iter().enumerate() {
        let hdr_off = sect_hdr_off + i * section_header_size;
        let name = section_name(sect.flags, i);
        pe[hdr_off..hdr_off + 8].copy_from_slice(&name);

        // VirtualSize
        pe[hdr_off + 8..hdr_off + 12].copy_from_slice(&sect.vsize.to_le_bytes());
        // VirtualAddress
        pe[hdr_off + 12..hdr_off + 16].copy_from_slice(&sect.rva.to_le_bytes());

        // SizeOfRawData (aligned)
        let raw_size = if sect.psize > 0 {
            ((sect.psize + file_alignment - 1) / file_alignment) * file_alignment
        } else {
            0
        };
        pe[hdr_off + 16..hdr_off + 20].copy_from_slice(&raw_size.to_le_bytes());

        // PointerToRawData
        if raw_size > 0 {
            pe[hdr_off + 20..hdr_off + 24].copy_from_slice(&file_offset.to_le_bytes());
        }

        // Characteristics (strip IMAGE_SCN_COMPRESSED flag)
        let clean_flags = sect.flags & !IMAGE_SCN_COMPRESSED;
        pe[hdr_off + 36..hdr_off + 40].copy_from_slice(&clean_flags.to_le_bytes());

        // Append section data
        if sect.psize > 0 {
            let is_compressed = sect.flags & IMAGE_SCN_COMPRESSED != 0;

            if is_compressed {
                // Decompress CECompress (LZX) data
                let data_offset = addr_to_offset(sect.data_ptr, virtual_base);
                match data_offset {
                    Ok(off) if off + sect.psize as usize <= data.len() => {
                        let compressed = &data[off..off + sect.psize as usize];
                        match cecompress::decompress(compressed, sect.vsize) {
                            Ok(decompressed) => {
                                let copy_len = decompressed.len().min(raw_size as usize);
                                pe.extend_from_slice(&decompressed[..copy_len]);
                                let pad = raw_size as usize - copy_len;
                                pe.resize(pe.len() + pad, 0);
                            }
                            Err(e) => {
                                eprintln!(
                                    "    Warning: CECompress decompression failed: {} - writing zeros",
                                    e
                                );
                                pe.resize(pe.len() + raw_size as usize, 0);
                            }
                        }
                    }
                    _ => {
                        pe.resize(pe.len() + raw_size as usize, 0);
                    }
                }
            } else {
                // Read data from the image
                let data_offset = addr_to_offset(sect.data_ptr, virtual_base);
                match data_offset {
                    Ok(off) if off + sect.psize as usize <= data.len() => {
                        pe.extend_from_slice(&data[off..off + sect.psize as usize]);
                        // Pad to alignment
                        let pad = raw_size as usize - sect.psize as usize;
                        pe.resize(pe.len() + pad, 0);
                    }
                    _ => {
                        pe.resize(pe.len() + raw_size as usize, 0);
                    }
                }
            }

            file_offset += raw_size;
        }
    }

    Ok(pe)
}

/// Extract all modules and files from a parsed ROM image
pub fn extract(data: &[u8], rom: &RomImage, output_dir: &Path) -> Result<()> {
    let modules_dir = output_dir.join("modules");
    let files_dir = output_dir.join("files");
    std::fs::create_dir_all(&modules_dir)?;
    std::fs::create_dir_all(&files_dir)?;

    // Extract modules
    for (name, entry) in &rom.modules {
        let safe_name = sanitize_filename(name);
        let out_path = modules_dir.join(&safe_name);

        match extract_module(data, entry, rom.virtual_base, rom.rom_hdr.cpu_type) {
            Ok(pe_data) => {
                std::fs::write(&out_path, &pe_data)?;
                println!("  [MODULE] {} ({} bytes)", safe_name, pe_data.len());
            }
            Err(e) => {
                eprintln!("  [MODULE] {} - FAILED: {}", safe_name, e);
            }
        }
    }

    // Extract files
    for (name, entry) in &rom.files {
        let safe_name = sanitize_filename(name);
        let out_path = files_dir.join(&safe_name);

        match extract_file(data, entry, rom.virtual_base) {
            Ok(file_data) => {
                std::fs::write(&out_path, &file_data)?;
                println!("  [FILE]   {} ({} bytes)", safe_name, file_data.len());
            }
            Err(e) => {
                eprintln!("  [FILE]   {} - FAILED: {}", safe_name, e);
            }
        }
    }

    Ok(())
}

/// Extract a module and reconstruct it as a PE file
fn extract_module(
    data: &[u8],
    entry: &TocEntry,
    virtual_base: u32,
    cpu_type: u16,
) -> Result<Vec<u8>> {
    let e32_off = addr_to_offset(entry.e32_offset, virtual_base)
        .context("Invalid e32 offset")?;
    let e32 = parse_e32_rom(data, e32_off)?;

    let mut sections = Vec::new();
    for i in 0..e32.obj_cnt as usize {
        let o32_off = addr_to_offset(entry.o32_offset, virtual_base)
            .context("Invalid o32 offset")?
            + i * 24;
        sections.push(parse_o32_rom(data, o32_off)?);
    }

    let has_compressed = sections.iter().any(|s| s.flags & IMAGE_SCN_COMPRESSED != 0);
    if has_compressed {
        eprintln!("    Note: some sections are compressed (CECompress) - decompressing");
    }

    build_pe(data, &e32, &sections, virtual_base, cpu_type)
}

/// Extract a data file
fn extract_file(data: &[u8], entry: &FileEntry, virtual_base: u32) -> Result<Vec<u8>> {
    let is_compressed = entry.comp_file_size != entry.real_file_size;

    let data_offset = addr_to_offset(entry.load_offset, virtual_base)
        .context("Invalid file load offset")?;
    let data_size = entry.comp_file_size as usize;

    if data_offset + data_size > data.len() {
        return Err(anyhow!(
            "File data extends beyond image (offset={:#x}, size={:#x})",
            data_offset,
            data_size
        ));
    }

    if is_compressed {
        let compressed = &data[data_offset..data_offset + data_size];
        match cecompress::decompress(compressed, entry.real_file_size) {
            Ok(decompressed) => {
                eprintln!(
                    "    Decompressed file ({} -> {} bytes)",
                    entry.comp_file_size, decompressed.len()
                );
                return Ok(decompressed);
            }
            Err(e) => {
                eprintln!(
                    "    Warning: decompression failed ({} -> {} bytes): {} - extracting raw",
                    entry.comp_file_size, entry.real_file_size, e
                );
            }
        }
    }

    Ok(data[data_offset..data_offset + data_size].to_vec())
}

/// Print information about a ROM image
pub fn print_info(rom: &RomImage) {
    println!("ROM Image Information:");
    println!("  Virtual base:  {:#010x}", rom.virtual_base);
    println!("  ROMHDR RVA:    {:#010x}", rom.hdr_rva);
    println!("  ROMHDR offset: {:#010x}", rom.hdr_offset);
    println!("  CPU type:      {:#06x} ({})", rom.rom_hdr.cpu_type, cpu_name(rom.rom_hdr.cpu_type));
    println!("  Phys first:    {:#010x}", rom.rom_hdr.phys_first);
    println!("  Phys last:     {:#010x}", rom.rom_hdr.phys_last);
    println!("  Modules:       {}", rom.rom_hdr.num_mods);
    println!("  Files:         {}", rom.rom_hdr.num_files);
    println!("  Kernel flags:  {:#010x}", rom.rom_hdr.kernel_flags);
    println!();

    if !rom.modules.is_empty() {
        println!("Modules:");
        for (name, entry) in &rom.modules {
            println!(
                "  {:40} {:>10} bytes  attr={:#06x}  load={:#010x}",
                name, entry.file_size, entry.file_attributes, entry.load_offset
            );
        }
        println!();
    }

    if !rom.files.is_empty() {
        println!("Files:");
        for (name, entry) in &rom.files {
            let comp_note = if entry.comp_file_size != entry.real_file_size {
                format!(
                    " (compressed: {} -> {})",
                    entry.comp_file_size, entry.real_file_size
                )
            } else {
                String::new()
            };
            println!(
                "  {:40} {:>10} bytes  attr={:#06x}{}",
                name, entry.real_file_size, entry.file_attributes, comp_note
            );
        }
    }
}

fn cpu_name(cpu_type: u16) -> &'static str {
    match cpu_type {
        0x01c0 => "ARM",
        0x01c2 => "ARM Thumb",
        0x01c4 => "ARMv7",
        0x0166 => "MIPS R4000",
        0x01f0 => "PowerPC",
        0x014c => "x86",
        0x0200 => "IA-64",
        _ => "Unknown",
    }
}

fn sanitize_filename(name: &str) -> String {
    name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_")
}
