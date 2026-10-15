# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

zunextract is a Rust CLI tool for extracting files from Zune HD (Windows CE 6.0) firmware images. It handles a three-layer firmware format: CAB archives containing B000FF-compressed BIN files, which decompress to NB0 raw images containing Windows CE XIP ROM filesystems.

## Build & Run

```bash
cargo build              # debug build
cargo build --release    # release build
cargo run -- <subcommand> [args]
cargo clippy             # lint
cargo fmt                # format
```

No test suite exists currently.

## Architecture

The processing pipeline follows the firmware's layered structure:

1. **`main.rs`** — CLI entry point using clap with subcommands: `info`, `extract`, `cab`, `bin`, `rom`, `explore`. Contains file type detection (CAB magic `MSCF`, B000FF signature, or raw binary) and orchestrates the pipeline.

2. **`b000ff.rs`** — Parses the B000FF binary format (Windows CE firmware container). The format is: 7-byte signature + image_start/image_length header + sequence of addressed data blocks (address, size, checksum, data) terminated by a zero block. Decompresses sparse block layout into a flat NB0 image.

3. **`rom.rs`** — Parses Windows CE XIP ROM images. Locates the ECEC signature (first at offset 0x40, then by scanning), reads ROMHDR, then iterates TOC entries (modules) and file entries. Modules are reconstructed as PE files via `build_pe()` which generates DOS/COFF/optional headers and maps o32_rom sections. All addresses are virtual (relative to `virtual_base = hdr_rva - hdr_offset`). Uses `cecompress` module to decompress compressed PE sections and files.

4. **`cecompress.rs`** — Implements CECompress (LZX) decompression used in Windows CE ROMs. Two-layer format: `CEDecompressROM` outer block table that splits data into independently decompressible blocks, each containing an LZX-compressed stream with a 16-byte header (window_size, decompressed_size, 8 reserved bytes). The LZX engine supports verbatim, aligned, and uncompressed block types with E8 (Intel CALL) post-processing.

5. **`tui.rs`** — Interactive terminal explorer using ratatui/crossterm. Loads firmware into memory, provides tabbed browsing (Info/Modules/Files) with extraction support. Uses `RomImageRef` as a Clone-friendly mirror of `RomImage`.

## Key Design Details

- Address translation everywhere uses `addr - virtual_base` to convert ROM virtual addresses to file offsets
- CECompress (LZX) decompression is implemented — compressed PE sections and files are decompressed automatically, with fallback to raw extraction on failure
- Module extraction rebuilds full PE32 binaries from e32_rom/o32_rom structures (not just raw data dumps)
- The TUI re-parses NB0 data on extraction rather than caching parsed ROM state
