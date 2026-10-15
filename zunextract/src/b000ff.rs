//! B000FF binary image format parser for Windows CE firmware files.
//!
//! The B000FF format is used by Windows CE devices to store firmware images.
//! It consists of a header followed by a series of addressed data blocks,
//! allowing sparse image representation (gaps are filled with 0xFF or 0x00).

use anyhow::{anyhow, Result};

const SIGNATURE: &[u8; 7] = b"B000FF\n";

/// Parsed B000FF header
#[derive(Debug)]
pub struct B000ffHeader {
    pub image_start: u32,
    pub image_length: u32,
}

/// Parse B000FF header from raw bytes
pub fn parse_header(data: &[u8]) -> Result<B000ffHeader> {
    if data.len() < 15 {
        return Err(anyhow!("File too small for B000FF header"));
    }
    if &data[..7] != SIGNATURE {
        return Err(anyhow!(
            "Invalid B000FF signature: expected {:02x?}, got {:02x?}",
            SIGNATURE,
            &data[..7]
        ));
    }
    let image_start = u32::from_le_bytes(data[7..11].try_into()?);
    let image_length = u32::from_le_bytes(data[11..15].try_into()?);
    Ok(B000ffHeader {
        image_start,
        image_length,
    })
}

/// Decompress a B000FF image into a flat raw (NB0) image.
///
/// The output image has all blocks placed at their correct offsets
/// relative to `image_start`. Gaps are filled with 0x00.
pub fn decompress(data: &[u8]) -> Result<(B000ffHeader, Vec<u8>)> {
    let header = parse_header(data)?;

    let mut out = vec![0u8; header.image_length as usize];
    let mut offset = 15; // Skip header
    let mut block_count = 0u32;

    loop {
        if offset + 12 > data.len() {
            return Err(anyhow!("Unexpected end of file while reading block header"));
        }

        let address = u32::from_le_bytes(data[offset..offset + 4].try_into()?);
        let size = u32::from_le_bytes(data[offset + 4..offset + 8].try_into()?);
        let checksum = u32::from_le_bytes(data[offset + 8..offset + 12].try_into()?);
        offset += 12;

        // Termination block: address and checksum are both 0
        if address == 0 && checksum == 0 {
            break;
        }

        if offset + size as usize > data.len() {
            return Err(anyhow!(
                "Block {} data extends beyond file (offset={:#x}, size={:#x}, file_len={:#x})",
                block_count,
                offset,
                size,
                data.len()
            ));
        }

        let block_data = &data[offset..offset + size as usize];
        offset += size as usize;

        // Verify checksum (byte sum)
        let computed_checksum: u32 = block_data.iter().map(|&b| b as u32).sum();
        if computed_checksum != checksum {
            eprintln!(
                "Warning: block {} checksum mismatch (expected {:#x}, got {:#x})",
                block_count, checksum, computed_checksum
            );
        }

        // Place data at correct offset in output
        let dest_offset = address
            .checked_sub(header.image_start)
            .ok_or_else(|| {
                anyhow!(
                    "Block address {:#x} is before image start {:#x}",
                    address,
                    header.image_start
                )
            })? as usize;

        if dest_offset + size as usize > out.len() {
            return Err(anyhow!(
                "Block {} extends beyond image (dest_offset={:#x}, size={:#x}, image_len={:#x})",
                block_count,
                dest_offset,
                size,
                out.len()
            ));
        }

        out[dest_offset..dest_offset + size as usize].copy_from_slice(block_data);
        block_count += 1;
    }

    eprintln!(
        "Decompressed {} blocks, image_start={:#x}, image_length={:#x}",
        block_count, header.image_start, header.image_length
    );

    Ok((header, out))
}

/// Check if a file starts with the B000FF signature
pub fn is_b000ff(data: &[u8]) -> bool {
    data.len() >= 7 && &data[..7] == SIGNATURE
}
