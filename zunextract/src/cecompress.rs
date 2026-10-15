//! CECompress (LZX) decompression for Windows CE ROM images.
//!
//! Implements the two-layer decompression used in Windows CE 6.0:
//! 1. `CEDecompressROM` — block table wrapper that splits compressed data into
//!    independently decompressible blocks
//! 2. LZX decompression — an LZ77 variant with Huffman coding used by Microsoft

use anyhow::{anyhow, Result};

/// Decompress CECompress data from a Windows CE ROM.
///
/// This is the outer `CEDecompressROM` function that manages the block table
/// and delegates each block to the LZX decompressor.
pub fn decompress(compressed: &[u8], uncompressed_size: u32) -> Result<Vec<u8>> {
    ce_decompress_rom(compressed, uncompressed_size, 0, 1, 4096)
}

fn read_u24_le(data: &[u8], offset: usize) -> u32 {
    data[offset] as u32 | ((data[offset + 1] as u32) << 8) | ((data[offset + 2] as u32) << 16)
}

fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

/// CEDecompressROM: block table parser and dispatcher.
fn ce_decompress_rom(
    data: &[u8],
    uncompressed_size: u32,
    skip: u32,
    step: u32,
    blocksize: u32,
) -> Result<Vec<u8>> {
    if step != 1 && step != 2 {
        return Err(anyhow!("CEDecompressROM: invalid step {}", step));
    }

    let block_bits = if blocksize == 4096 { 12 } else { 10 };

    if (skip & ((1 << block_bits) - 1)) != 0 || data.len() <= 2 {
        return Err(anyhow!("CEDecompressROM: invalid skip/data"));
    }

    let raw_num = read_u24_le(data, 0);
    let num_blocks = if raw_num == 0 {
        2usize
    } else {
        ((raw_num as usize - 1) >> block_bits) + 2
    };

    let table_end = num_blocks * 3;
    if table_end > data.len() {
        return Err(anyhow!("CEDecompressROM: block table exceeds data"));
    }

    let next_block = (skip >> block_bits) as usize;
    if next_block >= num_blocks {
        return Err(anyhow!("CEDecompressROM: skip past end of blocks"));
    }

    // Get starting compressed offset
    let mut comp_offset = if next_block != 0 {
        read_u24_le(data, next_block * 3) as usize
    } else {
        table_end
    };

    let mut output = vec![0u8; uncompressed_size as usize];
    let mut out_pos = 0usize;
    let mut remaining = uncompressed_size as usize;

    for block_idx in (next_block + 1)..num_blocks {
        if remaining == 0 {
            break;
        }

        let next_comp_offset = read_u24_le(data, block_idx * 3) as usize;
        if next_comp_offset < comp_offset || next_comp_offset > data.len() {
            return Err(anyhow!(
                "CEDecompressROM: invalid block offset at block {}",
                block_idx
            ));
        }

        let block_data = &data[comp_offset..next_comp_offset];
        let decompressed = bin_decompress_rom(block_data)?;
        let copy_len = decompressed.len().min(remaining);

        if step == 1 {
            output[out_pos..out_pos + copy_len].copy_from_slice(&decompressed[..copy_len]);
        } else {
            // step == 2: interleaved byte placement
            for (i, &b) in decompressed[..copy_len].iter().enumerate() {
                let dest = out_pos + i * step as usize;
                if dest < output.len() {
                    output[dest] = b;
                }
            }
        }

        out_pos += copy_len * step as usize;
        remaining -= copy_len;
        comp_offset = next_comp_offset;
    }

    output.truncate(uncompressed_size as usize);
    Ok(output)
}

/// Decompress a single LZX block with its 16-byte header.
fn bin_decompress_rom(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 16 {
        return Err(anyhow!("LZX block too small for header"));
    }

    let window_size = read_u32_le(data, 0);
    let decompressed_size = read_u32_le(data, 4);
    // Skip 8 bytes of reserved data
    let compressed_data = &data[16..];

    if !(15..=21).contains(&window_size) {
        return Err(anyhow!("LZX: invalid window size {}", window_size));
    }

    let mut decoder = LzxDecoder::new(window_size);
    decoder.decompress(compressed_data, decompressed_size as usize)
}

// ============================================================================
// LZX Decompression Engine
// ============================================================================

const NUM_CHARS: usize = 256;
const MIN_MATCH: usize = 2;
const NUM_PRIMARY_LENGTHS: u32 = 7;
const PRETREE_NUM_ELEMENTS: usize = 20;
const SECONDARY_NUM_ELEMENTS: usize = 249;
const ALIGNED_NUM_ELEMENTS: usize = 8;

const PRETREE_TABLEBITS: u32 = 6;
const MAINTREE_TABLEBITS: u32 = 11;
const LENTREE_TABLEBITS: u32 = 10;
const ALIGNTREE_TABLEBITS: u32 = 7;

const PRETREE_MAX_CODEWORD: u32 = 16;
const MAINTREE_MAX_CODEWORD: u32 = 16;
const LENTREE_MAX_CODEWORD: u32 = 16;
const ALIGNTREE_MAX_CODEWORD: u32 = 8;

const LENTABLE_SAFETY: usize = 64;

const BLOCKTYPE_VERBATIM: u32 = 1;
const BLOCKTYPE_ALIGNED: u32 = 2;
const BLOCKTYPE_UNCOMPRESSED: u32 = 3;

static EXTRA_BITS: [u8; 51] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
    13, 13, 14, 14, 15, 15, 16, 16, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17,
];

static POSITION_BASE: [u32; 51] = [
    0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536,
    2048, 3072, 4096, 6144, 8192, 12288, 16384, 24576, 32768, 49152, 65536, 98304, 131072,
    196608, 262144, 393216, 524288, 655360, 786432, 917504, 1048576, 1179648, 1310720, 1441792,
    1572864, 1703936, 1835008, 1966080, 2097152,
];

static POSITION_SLOTS: [usize; 11] = [30, 32, 34, 36, 38, 42, 50, 66, 98, 162, 290];

struct BitBuffer<'a> {
    data: &'a [u8],
    pos: usize,
    buffer: u32,
    bits_left: i32,
}

impl<'a> BitBuffer<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            buffer: 0,
            bits_left: 0,
        }
    }

    fn ensure_bits(&mut self, bits: i32) {
        while self.bits_left < bits {
            let lo = if self.pos < self.data.len() {
                let v = self.data[self.pos];
                self.pos += 1;
                v as u32
            } else {
                0
            };
            let hi = if self.pos < self.data.len() {
                let v = self.data[self.pos];
                self.pos += 1;
                v as u32
            } else {
                0
            };
            self.buffer |= ((hi << 8) | lo) << (32 - 16 - self.bits_left);
            self.bits_left += 16;
        }
    }

    fn peek_bits(&self, bits: u32) -> u32 {
        self.buffer >> (32 - bits)
    }

    fn remove_bits(&mut self, bits: u32) {
        self.buffer = self.buffer.wrapping_shl(bits);
        self.bits_left -= bits as i32;
    }

    fn read_bits(&mut self, bits: u32) -> u32 {
        if bits == 0 {
            return 0;
        }
        self.ensure_bits(bits as i32);
        let ret = self.peek_bits(bits);
        self.remove_bits(bits);
        ret
    }

    fn reset(&mut self) {
        self.buffer = 0;
        self.bits_left = 0;
    }

    /// Current byte position in the underlying data stream.
    fn byte_pos(&self) -> usize {
        self.pos
    }

    /// Seek to a byte position in the underlying data, resetting the bit buffer.
    fn seek(&mut self, pos: usize) {
        self.pos = pos;
        self.buffer = 0;
        self.bits_left = 0;
    }

    /// Read raw bytes directly, bypassing the bit buffer.
    fn read_bytes(&mut self, count: usize) -> Vec<u8> {
        // If we have buffered bits, we need to handle the byte alignment
        // For uncompressed blocks, the spec says to flush the bit buffer first
        let end = (self.pos + count).min(self.data.len());
        let result = self.data[self.pos..end].to_vec();
        self.pos = end;
        result
    }

    fn read_u32_le(&mut self) -> u32 {
        let bytes = self.read_bytes(4);
        if bytes.len() == 4 {
            u32::from_le_bytes(bytes.try_into().unwrap())
        } else {
            0
        }
    }
}

struct LzxDecoder {
    window_size: usize,
    window: Vec<u8>,
    window_posn: usize,
    r0: u32,
    r1: u32,
    r2: u32,
    main_elements: usize,
    header_read: bool,
    block_type: u32,
    block_remaining: usize,
    intel_filesize: i32,
    intel_started: bool,

    pretree_len: Vec<u8>,
    pretree_table: Vec<u16>,
    maintree_len: Vec<u8>,
    maintree_table: Vec<u16>,
    lentree_len: Vec<u8>,
    lentree_table: Vec<u16>,
    aligntree_len: Vec<u8>,
    aligntree_table: Vec<u16>,
}

impl LzxDecoder {
    fn new(window_bits: u32) -> Self {
        let window_size = 1usize << window_bits;
        let slot_idx = (window_bits - 15) as usize;
        let main_elements = NUM_CHARS + (POSITION_SLOTS[slot_idx] << 3);

        let maintree_maxsymbols = NUM_CHARS + (51 << 3);

        Self {
            window_size,
            window: vec![0xDC; window_size],
            window_posn: 0,
            r0: 1,
            r1: 1,
            r2: 1,
            main_elements,
            header_read: false,
            block_type: 0,
            block_remaining: 0,
            intel_filesize: 0,
            intel_started: false,

            pretree_len: vec![0; PRETREE_NUM_ELEMENTS + LENTABLE_SAFETY],
            pretree_table: vec![0; (1 << PRETREE_TABLEBITS) + (PRETREE_NUM_ELEMENTS * 2)],
            maintree_len: vec![0; maintree_maxsymbols + LENTABLE_SAFETY],
            maintree_table: vec![0; (1 << MAINTREE_TABLEBITS) + (maintree_maxsymbols * 2)],
            lentree_len: vec![0; SECONDARY_NUM_ELEMENTS + LENTABLE_SAFETY],
            lentree_table: vec![0; (1 << LENTREE_TABLEBITS) + (SECONDARY_NUM_ELEMENTS * 2)],
            aligntree_len: vec![0; ALIGNED_NUM_ELEMENTS + LENTABLE_SAFETY],
            aligntree_table: vec![0; (1 << ALIGNTREE_TABLEBITS) + (ALIGNED_NUM_ELEMENTS * 2)],
        }
    }

    fn decompress(&mut self, data: &[u8], out_len: usize) -> Result<Vec<u8>> {
        let mut bits = BitBuffer::new(data);
        let mut togo = out_len;

        // Read header
        if !self.header_read {
            let intel = bits.read_bits(1);
            if intel == 1 {
                let i = bits.read_bits(16);
                let j = bits.read_bits(16);
                self.intel_filesize = ((i << 16) | j) as i32;
            }
            self.header_read = true;
        }

        while togo > 0 {
            if self.block_remaining == 0 {
                if self.block_type == BLOCKTYPE_UNCOMPRESSED {
                    // Align to 16-bit boundary after odd-length uncompressed block
                    // The Python reference does: in_f.seek(1, os.SEEK_CUR)
                    // We need to skip a padding byte if block_length was odd
                    // Note: block_length was already consumed, but we need to check
                    // if we need to skip a byte for alignment
                    if (bits.byte_pos() & 1) != 0 {
                        bits.seek(bits.byte_pos() + 1);
                    }
                    bits.reset();
                }

                self.block_type = bits.read_bits(3);
                let block_length = bits.read_bits(24) as usize;
                self.block_remaining = block_length;

                match self.block_type {
                    BLOCKTYPE_ALIGNED => {
                        for i in 0..8 {
                            self.aligntree_len[i] = bits.read_bits(3) as u8;
                        }
                        make_decode_table(
                            ALIGNED_NUM_ELEMENTS,
                            ALIGNTREE_TABLEBITS,
                            &self.aligntree_len,
                            &mut self.aligntree_table,
                        )?;

                        self.read_lengths(&mut bits, 0, 256, true)?;
                        self.read_lengths(&mut bits, 256, self.main_elements, true)?;
                        make_decode_table(
                            NUM_CHARS + (51 << 3),
                            MAINTREE_TABLEBITS,
                            &self.maintree_len,
                            &mut self.maintree_table,
                        )?;
                        if self.maintree_len[0xE8] != 0 {
                            self.intel_started = true;
                        }

                        self.read_lengths(&mut bits, 0, SECONDARY_NUM_ELEMENTS, false)?;
                        make_decode_table(
                            SECONDARY_NUM_ELEMENTS,
                            LENTREE_TABLEBITS,
                            &self.lentree_len,
                            &mut self.lentree_table,
                        )?;
                    }
                    BLOCKTYPE_VERBATIM => {
                        self.read_lengths(&mut bits, 0, 256, true)?;
                        self.read_lengths(&mut bits, 256, self.main_elements, true)?;
                        make_decode_table(
                            NUM_CHARS + (51 << 3),
                            MAINTREE_TABLEBITS,
                            &self.maintree_len,
                            &mut self.maintree_table,
                        )?;
                        if self.maintree_len[0xE8] != 0 {
                            self.intel_started = true;
                        }

                        self.read_lengths(&mut bits, 0, SECONDARY_NUM_ELEMENTS, false)?;
                        make_decode_table(
                            SECONDARY_NUM_ELEMENTS,
                            LENTREE_TABLEBITS,
                            &self.lentree_len,
                            &mut self.lentree_table,
                        )?;
                    }
                    BLOCKTYPE_UNCOMPRESSED => {
                        self.intel_started = true;
                        bits.ensure_bits(16);
                        if bits.bits_left > 16 {
                            // Rewind 2 bytes
                            bits.pos -= 2;
                        }
                        bits.reset();
                        self.r0 = bits.read_u32_le();
                        self.r1 = bits.read_u32_le();
                        self.r2 = bits.read_u32_le();
                    }
                    _ => {
                        return Err(anyhow!("LZX: invalid block type {}", self.block_type));
                    }
                }
            }

            let this_run = self.block_remaining.min(togo);
            togo -= this_run;

            self.window_posn &= self.window_size - 1;
            if self.window_posn + self.block_remaining > self.window_size {
                return Err(anyhow!("LZX: window position overflow"));
            }

            match self.block_type {
                BLOCKTYPE_VERBATIM => {
                    self.decompress_verbatim(&mut bits, this_run)?;
                }
                BLOCKTYPE_ALIGNED => {
                    self.decompress_aligned(&mut bits, this_run)?;
                }
                BLOCKTYPE_UNCOMPRESSED => {
                    let bytes = bits.read_bytes(this_run);
                    for (i, &b) in bytes.iter().enumerate() {
                        self.window[self.window_posn + i] = b;
                    }
                    self.window_posn += this_run;
                    self.block_remaining -= this_run;
                }
                _ => {
                    return Err(anyhow!("LZX: invalid block type"));
                }
            }
        }

        // Extract output from window
        let start = if self.window_posn == 0 {
            self.window_size
        } else {
            self.window_posn
        } - out_len;

        let mut output = self.window[start..start + out_len].to_vec();

        // Undo E8 preprocessing (Intel CALL translation)
        // CE variant: E8 preprocessing is NOT disabled after chunk 32768
        if out_len >= 10 && self.intel_started && self.intel_filesize > 0 {
            let mut i = 0;
            while i < out_len - 10 {
                if output[i] == 0xE8 {
                    let abs_offset = i32::from_le_bytes(
                        output[i + 1..i + 5].try_into().unwrap(),
                    );
                    let cur_pos = i as i32;
                    if abs_offset >= -cur_pos && abs_offset < self.intel_filesize {
                        let rel_offset = if abs_offset >= 0 {
                            abs_offset - cur_pos
                        } else {
                            abs_offset + self.intel_filesize
                        };
                        output[i + 1..i + 5].copy_from_slice(&rel_offset.to_le_bytes());
                    }
                    i += 5;
                } else {
                    i += 1;
                }
            }
        }

        Ok(output)
    }

    fn decompress_verbatim(&mut self, bits: &mut BitBuffer, mut count: usize) -> Result<()> {
        while count > 0 {
            let main_element = read_huff_sym(
                &self.maintree_table,
                &self.maintree_len,
                NUM_CHARS + (51 << 3),
                MAINTREE_TABLEBITS,
                bits,
                MAINTREE_MAX_CODEWORD,
            )?;

            if main_element < NUM_CHARS as u32 {
                self.window[self.window_posn] = main_element as u8;
                self.window_posn += 1;
                self.block_remaining -= 1;
                count -= 1;
                continue;
            }

            let main_element = main_element - NUM_CHARS as u32;
            let mut match_length = (main_element & NUM_PRIMARY_LENGTHS) as usize;
            if match_length == NUM_PRIMARY_LENGTHS as usize {
                let length_footer = read_huff_sym(
                    &self.lentree_table,
                    &self.lentree_len,
                    SECONDARY_NUM_ELEMENTS,
                    LENTREE_TABLEBITS,
                    bits,
                    LENTREE_MAX_CODEWORD,
                )?;
                match_length += length_footer as usize;
            }
            match_length += MIN_MATCH;

            let match_offset_slot = (main_element >> 3) as usize;
            let match_offset = if match_offset_slot > 2 {
                let extra = EXTRA_BITS[match_offset_slot] as u32;
                let verbatim_bits = bits.read_bits(extra);
                let offset =
                    POSITION_BASE[match_offset_slot] + verbatim_bits;
                self.r2 = self.r1;
                self.r1 = self.r0;
                self.r0 = offset - 2;
                self.r0
            } else if match_offset_slot == 0 {
                self.r0
            } else if match_offset_slot == 1 {
                std::mem::swap(&mut self.r0, &mut self.r1);
                self.r0
            } else {
                // slot == 2
                std::mem::swap(&mut self.r0, &mut self.r2);
                self.r0
            };

            self.copy_match(match_offset as usize, match_length);
            self.block_remaining -= match_length;
            count -= match_length;
        }
        Ok(())
    }

    fn decompress_aligned(&mut self, bits: &mut BitBuffer, mut count: usize) -> Result<()> {
        while count > 0 {
            let main_element = read_huff_sym(
                &self.maintree_table,
                &self.maintree_len,
                NUM_CHARS + (51 << 3),
                MAINTREE_TABLEBITS,
                bits,
                MAINTREE_MAX_CODEWORD,
            )?;

            if main_element < NUM_CHARS as u32 {
                self.window[self.window_posn] = main_element as u8;
                self.window_posn += 1;
                self.block_remaining -= 1;
                count -= 1;
                continue;
            }

            let main_element = main_element - NUM_CHARS as u32;
            let mut match_length = (main_element & NUM_PRIMARY_LENGTHS) as usize;
            if match_length == NUM_PRIMARY_LENGTHS as usize {
                let length_footer = read_huff_sym(
                    &self.lentree_table,
                    &self.lentree_len,
                    SECONDARY_NUM_ELEMENTS,
                    LENTREE_TABLEBITS,
                    bits,
                    LENTREE_MAX_CODEWORD,
                )?;
                match_length += length_footer as usize;
            }
            match_length += MIN_MATCH;

            let match_offset_slot = (main_element >> 3) as usize;
            let match_offset = if match_offset_slot > 2 {
                let extra = EXTRA_BITS[match_offset_slot] as u32;
                let (verbatim_bits, aligned_bits) = if extra >= 3 {
                    let vb = bits.read_bits(extra - 3) << 3;
                    let ab = read_huff_sym(
                        &self.aligntree_table,
                        &self.aligntree_len,
                        ALIGNED_NUM_ELEMENTS,
                        ALIGNTREE_TABLEBITS,
                        bits,
                        ALIGNTREE_MAX_CODEWORD,
                    )?;
                    (vb, ab)
                } else {
                    (bits.read_bits(extra), 0)
                };
                let offset = POSITION_BASE[match_offset_slot] + verbatim_bits + aligned_bits;
                self.r2 = self.r1;
                self.r1 = self.r0;
                self.r0 = offset - 2;
                self.r0
            } else if match_offset_slot == 0 {
                self.r0
            } else if match_offset_slot == 1 {
                std::mem::swap(&mut self.r0, &mut self.r1);
                self.r0
            } else {
                std::mem::swap(&mut self.r0, &mut self.r2);
                self.r0
            };

            self.copy_match(match_offset as usize, match_length);
            self.block_remaining -= match_length;
            count -= match_length;
        }
        Ok(())
    }

    fn copy_match(&mut self, match_offset: usize, match_length: usize) {
        let mut rundest = self.window_posn;
        let mut runsrc;
        let mut remaining = match_length;

        if self.window_posn >= match_offset {
            runsrc = rundest - match_offset;
        } else {
            runsrc = rundest + (self.window_size - match_offset);
            let copy_length = match_offset - self.window_posn;
            if copy_length < remaining {
                remaining -= copy_length;
                self.window_posn += copy_length;
                for _ in 0..copy_length {
                    self.window[rundest] = self.window[runsrc];
                    rundest += 1;
                    runsrc += 1;
                }
                runsrc = 0;
            }
        }

        self.window_posn += remaining;
        // Byte-by-byte copy (handles overlapping matches)
        for _ in 0..remaining {
            self.window[rundest] = self.window[runsrc];
            rundest += 1;
            runsrc += 1;
        }
    }

    /// Read pretree, then use it to decode length values.
    fn read_lengths(
        &mut self,
        bits: &mut BitBuffer,
        first: usize,
        last: usize,
        is_main: bool,
    ) -> Result<()> {
        // Read pretree
        for i in 0..20 {
            self.pretree_len[i] = bits.read_bits(4) as u8;
        }
        make_decode_table(
            PRETREE_NUM_ELEMENTS,
            PRETREE_TABLEBITS,
            &self.pretree_len,
            &mut self.pretree_table,
        )?;

        let lens = if is_main {
            &mut self.maintree_len
        } else {
            &mut self.lentree_len
        };

        let mut x = first;
        while x < last {
            let z = read_huff_sym(
                &self.pretree_table,
                &self.pretree_len,
                PRETREE_NUM_ELEMENTS,
                PRETREE_TABLEBITS,
                bits,
                PRETREE_MAX_CODEWORD,
            )?;

            if z == 17 {
                // Run of zeros (4-19)
                let y = bits.read_bits(4) as usize + 4;
                for _ in 0..y {
                    if x >= last {
                        break;
                    }
                    lens[x] = 0;
                    x += 1;
                }
            } else if z == 18 {
                // Longer run of zeros (20-51)
                let y = bits.read_bits(5) as usize + 20;
                for _ in 0..y {
                    if x >= last {
                        break;
                    }
                    lens[x] = 0;
                    x += 1;
                }
            } else if z == 19 {
                // Run of same value (4-5)
                let y = bits.read_bits(1) as usize + 4;
                let z2 = read_huff_sym(
                    &self.pretree_table,
                    &self.pretree_len,
                    PRETREE_NUM_ELEMENTS,
                    PRETREE_TABLEBITS,
                    bits,
                    PRETREE_MAX_CODEWORD,
                )?;
                let val = ((lens[x] as u32 + 17 - z2) % 17) as u8;
                for _ in 0..y {
                    if x >= last {
                        break;
                    }
                    lens[x] = val;
                    x += 1;
                }
            } else {
                // Single value delta
                lens[x] = ((lens[x] as u32 + 17 - z) % 17) as u8;
                x += 1;
            }
        }

        Ok(())
    }
}

/// Read a Huffman symbol from the bitstream.
fn read_huff_sym(
    table: &[u16],
    lengths: &[u8],
    nsyms: usize,
    nbits: u32,
    bits: &mut BitBuffer,
    max_codeword: u32,
) -> Result<u32> {
    bits.ensure_bits(max_codeword as i32);
    let mut i = table[bits.peek_bits(nbits) as usize] as usize;

    if i >= nsyms {
        let mut j = 1u32 << (32 - nbits);
        loop {
            j >>= 1;
            if j == 0 {
                return Ok(0);
            }
            i <<= 1;
            if (bits.buffer & j) != 0 {
                i |= 1;
            }
            if i >= table.len() {
                return Err(anyhow!("LZX: Huffman decode overflow"));
            }
            i = table[i] as usize;
            if i < nsyms {
                break;
            }
        }
    }

    let len = lengths[i] as u32;
    bits.remove_bits(len);
    Ok(i as u32)
}

/// Build a canonical Huffman decoding table.
#[allow(clippy::needless_range_loop)]
fn make_decode_table(
    nsyms: usize,
    nbits: u32,
    lengths: &[u8],
    table: &mut [u16],
) -> Result<()> {
    let table_mask = 1usize << nbits;
    let mut bit_num = 1u32;
    let mut pos = 0usize;
    let mut bit_mask = table_mask >> 1;
    let mut next_symbol = bit_mask;

    while bit_num <= nbits {
        for sym in 0..nsyms {
            if lengths[sym] as u32 == bit_num {
                let leaf = pos;
                pos += bit_mask;
                if pos > table_mask {
                    return Err(anyhow!("LZX: Huffman table overflow"));
                }
                for entry in &mut table[leaf..pos] {
                    *entry = sym as u16;
                }
            }
        }
        bit_mask >>= 1;
        bit_num += 1;
    }

    if pos != table_mask {
        for entry in &mut table[pos..table_mask] {
            *entry = 0;
        }

        pos <<= 16;
        let table_mask_shifted = table_mask << 16;
        let mut bit_mask = 1u32 << 15;

        while bit_num <= 16 {
            for sym in 0..nsyms {
                if lengths[sym] as u32 == bit_num {
                    let mut leaf = pos >> 16;
                    for fill in 0..(bit_num - nbits) {
                        if table[leaf] == 0 {
                            table[next_symbol * 2] = 0;
                            table[next_symbol * 2 + 1] = 0;
                            table[leaf] = next_symbol as u16;
                            next_symbol += 1;
                        }
                        leaf = (table[leaf] as usize) * 2;
                        if ((pos >> (15 - fill)) & 1) == 1 {
                            leaf += 1;
                        }
                    }
                    table[leaf] = sym as u16;
                    pos += bit_mask as usize;
                    if pos > table_mask_shifted {
                        return Err(anyhow!("LZX: Huffman table overflow (long codes)"));
                    }
                }
            }
            bit_mask >>= 1;
            bit_num += 1;
        }
    }

    // Verify: if pos != table_mask (shifted or not), check all symbols are zero-length
    if pos != table_mask && pos != (table_mask << 16) && lengths[..nsyms].iter().any(|&l| l != 0) {
        return Err(anyhow!("LZX: incomplete Huffman code"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_u24_le() {
        assert_eq!(read_u24_le(&[0x01, 0x02, 0x03], 0), 0x030201);
        assert_eq!(read_u24_le(&[0xFF, 0xFF, 0xFF], 0), 0xFFFFFF);
        assert_eq!(read_u24_le(&[0x00, 0x00, 0x00], 0), 0);
    }

    #[test]
    fn test_invalid_window_size() {
        // Window size 14 is too small
        let data = [14u8, 0, 0, 0, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(bin_decompress_rom(&data).is_err());

        // Window size 22 is too large
        let data = [22u8, 0, 0, 0, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(bin_decompress_rom(&data).is_err());
    }

    #[test]
    fn test_decompress_empty_input() {
        // Too small for header
        assert!(bin_decompress_rom(&[0; 4]).is_err());
    }
}
