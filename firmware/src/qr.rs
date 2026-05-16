//! Minimal no-std QR-code generator.
//!
//! # Capabilities
//! - **Versions** 1 – 5 (21 × 21 … 37 × 37 modules)
//! - **Mode**: byte (ISO-8859-1 / any arbitrary bytes)
//! - **Error correction level**: M (≈ 15 % recovery)
//! - **No heap** — all buffers are stack-allocated fixed-size arrays
//!
//! The generator is designed to encode Grocy codes such as `"grcy-p-123"`
//! (≤ 10 bytes) at version 1 and Grocy product URLs (≤ 84 bytes) at version 5.
//!
//! # Usage
//! ```no_run
//! use etag::qr::QrCode;
//! if let Some(qr) = QrCode::encode(b"grcy-p-42") {
//!     for row in 0..qr.size() {
//!         for col in 0..qr.size() {
//!             if qr.module(row, col) { /* dark */ }
//!         }
//!     }
//! }
//! ```

// ─────────────────────────────────────────────────────────────────────────────
// Version / capacity tables (ECC level M)
// ─────────────────────────────────────────────────────────────────────────────

/// Maximum supported version (5 → 37 × 37 modules).
pub const MAX_VERSION: usize = 5;
/// Module dimension for the maximum supported version.
pub const MAX_DIM: usize = 17 + 4 * MAX_VERSION; // 37

/// Per-version constants for ECC level M.
/// Fields: (max_user_bytes, data_codewords, ec_per_block, num_blocks)
///
/// "data_codewords" is the total number of data codewords (message bits ÷ 8).
/// "max_user_bytes" is the actual payload capacity after the 2-byte overhead
/// (4-bit mode indicator + 8-bit character count).
#[rustfmt::skip]
const VERSION_TABLE: [(u8, u8, u8, u8); MAX_VERSION] = [
    // v  max_usr  data_cw  ec_per_blk  blocks
    /* 1 */ (14,  16, 10, 1),
    /* 2 */ (26,  28, 16, 1),
    /* 3 */ (42,  44, 26, 1),
    /* 4 */ (62,  64, 18, 2),
    /* 5 */ (84,  86, 24, 2),
];

/// Alignment-pattern centre coordinates for each version (v1 has none).
/// Index 0 = version 1, …
#[rustfmt::skip]
const ALIGNMENT_POSITIONS: [&[u8]; MAX_VERSION] = [
    &[],        // v1 – no alignment pattern
    &[6, 18],   // v2
    &[6, 22],   // v3
    &[6, 26],   // v4
    &[6, 30],   // v5
];

// ─────────────────────────────────────────────────────────────────────────────
// Format information (ECC M, masks 0-7) — 15-bit strings XOR'd with 101010000010010
// Source: ISO/IEC 18004:2015, Table C.1 (re-ordered for mask index 0-7)
// ─────────────────────────────────────────────────────────────────────────────
#[rustfmt::skip]
const FORMAT_BITS_ECC_M: [u16; 8] = [
    0x5412, // mask 0
    0x5125, // mask 1
    0x5E7C, // mask 2
    0x5B4B, // mask 3
    0x45F9, // mask 4
    0x40CE, // mask 5
    0x4F97, // mask 6
    0x4AA0, // mask 7
];

// ─────────────────────────────────────────────────────────────────────────────
// Module flags stored in the module grid
// ─────────────────────────────────────────────────────────────────────────────

/// Bit 0: module is dark (black).
const DARK: u8 = 0x01;
/// Bit 1: module is part of a function pattern (not touched by masking or data).
const FUNC: u8 = 0x02;

// ─────────────────────────────────────────────────────────────────────────────
// GF(256) arithmetic — primitive polynomial x^8 + x^4 + x^3 + x^2 + 1 (0x11D)
// ─────────────────────────────────────────────────────────────────────────────

/// GF(256) context — exponent and logarithm tables.
struct Gf {
    /// exp[i] = α^i.  Doubled (512 entries) to avoid modular reduction.
    exp: [u8; 512],
    /// log[x] = i such that α^i = x.  log[0] is undefined.
    log: [u8; 256],
}

impl Gf {
    fn new() -> Self {
        let mut exp = [0u8; 512];
        let mut log = [0u8; 256];
        let mut x = 1u16;
        for i in 0..255usize {
            exp[i] = x as u8;
            exp[i + 255] = x as u8;
            log[x as usize] = i as u8;
            x <<= 1;
            if x & 0x100 != 0 {
                x ^= 0x11D;
            }
        }
        // Convenience: wrap exp[255] = exp[0] = 1
        exp[255] = exp[0];
        Self { exp, log }
    }

    #[inline(always)]
    fn mul(&self, a: u8, b: u8) -> u8 {
        if a == 0 || b == 0 {
            return 0;
        }
        self.exp[(self.log[a as usize] as usize) + (self.log[b as usize] as usize)]
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Reed-Solomon generator polynomial & remainder
// ─────────────────────────────────────────────────────────────────────────────

/// Compute the RS generator polynomial of the given `degree` (= number of EC
/// codewords).  `out` must have exactly `degree` elements.  The polynomial is
/// stored with the coefficient of x^(degree-1) at index 0, down to the
/// constant term at index degree-1 (i.e. highest power first, leading 1 omitted).
fn rs_divisor(degree: usize, gf: &Gf, out: &mut [u8]) {
    // Initialise as the polynomial "1" shifted to [0, …, 0, 1]
    for b in out.iter_mut() {
        *b = 0;
    }
    out[degree - 1] = 1;

    // Multiply by (x + α^i) for i in 0..degree
    let mut root = 1u8; // α^0
    for _ in 0..degree {
        for j in 0..degree {
            out[j] = gf.mul(out[j], root);
            if j + 1 < degree {
                out[j] ^= out[j + 1];
            }
        }
        root = gf.mul(root, 2); // advance root by α
    }
}

/// Divide `data` by `divisor` (highest power first, leading 1 omitted) and
/// write the `divisor.len()` remainder bytes into `remainder`.
fn rs_remainder(data: &[u8], divisor: &[u8], gf: &Gf, remainder: &mut [u8]) {
    let n = divisor.len();
    for b in remainder.iter_mut() {
        *b = 0;
    }
    for &byte in data {
        let factor = byte ^ remainder[0];
        remainder.copy_within(1.., 0);
        remainder[n - 1] = 0;
        if factor != 0 {
            for (r, &d) in remainder.iter_mut().zip(divisor.iter()) {
                *r ^= gf.mul(factor, d);
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// QrCode struct
// ─────────────────────────────────────────────────────────────────────────────

/// A generated QR code, stored as a square grid of modules.
///
/// Each module can be queried with [`QrCode::module`].  Dark modules correspond
/// to the black squares, light modules to the white ones.
pub struct QrCode {
    version: u8,
    size: u8,
    /// Module grid: `grid[row * MAX_DIM + col]`.
    /// Bit 0: dark flag.  Bit 1: function-pattern flag (not meaningful to callers).
    grid: [u8; MAX_DIM * MAX_DIM],
}

impl QrCode {
    // ── Public API ────────────────────────────────────────────────────────────

    /// Encode `data` as a QR code (byte mode, ECC level M).
    ///
    /// Returns `None` if `data` is longer than 84 bytes (version 5 capacity).
    pub fn encode(data: &[u8]) -> Option<Self> {
        // Select the minimum version that fits the payload.
        let version = Self::select_version(data.len())?;

        let mut qr = QrCode {
            version,
            size: (17 + 4 * version) as u8,
            grid: [0u8; MAX_DIM * MAX_DIM],
        };

        // Build the complete codeword sequence (data + ECC, interleaved).
        let mut codewords = [0u8; 134]; // v5 M has 134 total codewords — largest we handle
        let cw_count = qr.build_codewords(data, &mut codewords);

        // Place all function patterns (finder, separator, timing, alignment, dark module).
        qr.draw_function_patterns();
        // Reserve format info areas (set to function pattern, value filled later).
        qr.reserve_format_areas();

        // Place the codeword data modules.
        qr.place_codewords(&codewords[..cw_count]);

        // Evaluate all 8 masks and keep the one with the lowest penalty score.
        let best_mask = qr.choose_mask();
        qr.apply_mask(best_mask);
        qr.draw_format_bits(best_mask);

        Some(qr)
    }

    /// Version number (1 – 5).
    #[inline]
    pub fn version(&self) -> u8 {
        self.version
    }

    /// Side length of the QR code in modules (21 – 37).
    #[inline]
    pub fn size(&self) -> usize {
        self.size as usize
    }

    /// Returns `true` if the module at (`row`, `col`) is dark (black).
    ///
    /// # Panics
    /// Panics if `row` or `col` ≥ [`QrCode::size`].
    #[inline]
    pub fn module(&self, row: usize, col: usize) -> bool {
        assert!(row < self.size as usize && col < self.size as usize);
        self.grid[row * MAX_DIM + col] & DARK != 0
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn select_version(data_len: usize) -> Option<u8> {
        for (i, &(max, _, _, _)) in VERSION_TABLE.iter().enumerate() {
            if data_len <= max as usize {
                return Some((i + 1) as u8);
            }
        }
        None
    }

    /// Get/set helpers for the raw module grid.
    #[inline]
    fn get(&self, row: usize, col: usize) -> u8 {
        self.grid[row * MAX_DIM + col]
    }

    #[inline]
    fn set(&mut self, row: usize, col: usize, flags: u8) {
        self.grid[row * MAX_DIM + col] = flags;
    }

    #[inline]
    fn set_func(&mut self, row: usize, col: usize, dark: bool) {
        self.set(row, col, FUNC | if dark { DARK } else { 0 });
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Data / EC codeword assembly
    // ─────────────────────────────────────────────────────────────────────────

    /// Build the interleaved sequence of data + EC codewords.
    /// Returns the number of codewords written into `out`.
    fn build_codewords(&self, data: &[u8], out: &mut [u8]) -> usize {
        let vi = (self.version - 1) as usize;
        let (_, data_cw_total, ec_per_block, num_blocks) = VERSION_TABLE[vi];
        let data_cw_total = data_cw_total as usize;
        let ec_per_block = ec_per_block as usize;
        let num_blocks = num_blocks as usize;

        // ── Step 1: Encode in byte mode into a flat data codeword array ───────
        // Layout: [mode_indicator 4b][char_count 8b][data bytes][terminator][padding]
        let mut data_buf = [0u8; 86]; // max data_cw_total is 86
        let bit_capacity = data_cw_total * 8;
        let mut bit_pos = 0usize;

        let write_bits = |buf: &mut [u8], pos: &mut usize, value: u32, count: usize| {
            for i in (0..count).rev() {
                let bit = (value >> i) & 1;
                let byte_idx = *pos / 8;
                let bit_idx = 7 - (*pos % 8);
                buf[byte_idx] |= (bit as u8) << bit_idx;
                *pos += 1;
            }
        };

        // Mode indicator: 0100 = byte mode
        write_bits(&mut data_buf, &mut bit_pos, 0b0100, 4);
        // Character count (8 bits for versions 1-9, byte mode)
        write_bits(&mut data_buf, &mut bit_pos, data.len() as u32, 8);
        // Data bytes
        for &b in data {
            write_bits(&mut data_buf, &mut bit_pos, b as u32, 8);
        }
        // Terminator (up to 4 zero bits)
        let remaining = bit_capacity - bit_pos;
        let term_bits = remaining.min(4);
        write_bits(&mut data_buf, &mut bit_pos, 0, term_bits);
        // Pad to byte boundary
        let pad_to_byte = (8 - bit_pos % 8) % 8;
        write_bits(&mut data_buf, &mut bit_pos, 0, pad_to_byte);
        // Pad codewords alternating 0xEC / 0x11
        let mut pad_byte = 0xEC_u8;
        while bit_pos < bit_capacity {
            write_bits(&mut data_buf, &mut bit_pos, pad_byte as u32, 8);
            pad_byte ^= 0xEC ^ 0x11; // toggles between 0xEC and 0x11
        }

        // ── Step 2: Split data into blocks and compute EC for each ────────────
        // For all versions we support, block sizes are equal or differ by 1.
        // data_cw_total / num_blocks gives the base block size; the remainder
        // goes into the first groups.
        let base_data_per_block = data_cw_total / num_blocks;
        let extra_blocks = data_cw_total % num_blocks; // blocks with base+1 codewords

        // max block data size
        let max_block_data = base_data_per_block + if extra_blocks > 0 { 1 } else { 0 };

        let mut block_data = [[0u8; 44]; 2]; // max 2 blocks; V3 M has 44 data cw in one block
        let mut block_ec = [[0u8; 26]; 2];   // max 2 blocks, max 26 ec cw (v3)
        let mut block_sizes = [0usize; 2];

        let gf = Gf::new();
        let mut gen = [0u8; 26]; // max ec_per_block = 26 (v3 M)
        rs_divisor(ec_per_block, &gf, &mut gen[..ec_per_block]);

        let mut data_offset = 0;
        for b in 0..num_blocks {
            let this_block_data = base_data_per_block + if b < extra_blocks { 1 } else { 0 };
            block_sizes[b] = this_block_data;
            block_data[b][..this_block_data]
                .copy_from_slice(&data_buf[data_offset..data_offset + this_block_data]);
            data_offset += this_block_data;

            let mut rem = [0u8; 26];
            rs_remainder(
                &block_data[b][..this_block_data],
                &gen[..ec_per_block],
                &gf,
                &mut rem[..ec_per_block],
            );
            block_ec[b][..ec_per_block].copy_from_slice(&rem[..ec_per_block]);
        }

        // ── Step 3: Interleave blocks ─────────────────────────────────────────
        let mut idx = 0usize;
        for i in 0..max_block_data {
            for b in 0..num_blocks {
                if i < block_sizes[b] {
                    out[idx] = block_data[b][i];
                    idx += 1;
                }
            }
        }
        for i in 0..ec_per_block {
            for b in 0..num_blocks {
                out[idx] = block_ec[b][i];
                idx += 1;
            }
        }
        idx
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Function pattern drawing
    // ─────────────────────────────────────────────────────────────────────────

    fn draw_function_patterns(&mut self) {
        let size = self.size as usize;

        // Finder patterns + separators at the three corners.
        self.draw_finder(0, 0);
        self.draw_finder(0, size - 7);
        self.draw_finder(size - 7, 0);

        // Timing patterns (row 6 and col 6)
        for i in 8..size - 8 {
            let dark = i % 2 == 0;
            self.set_func(6, i, dark);
            self.set_func(i, 6, dark);
        }

        // Alignment patterns (version 2+)
        let positions = ALIGNMENT_POSITIONS[(self.version - 1) as usize];
        for &r in positions {
            for &c in positions {
                // Skip if the centre would overlap a finder pattern corner.
                if !self.is_finder_region(r as usize, c as usize) {
                    self.draw_alignment(r as usize, c as usize);
                }
            }
        }

        // Dark module — always set at (4*version+9, 8).
        let dark_row = 4 * (self.version as usize) + 9;
        self.set_func(dark_row, 8, true);
    }

    /// Draw a 7 × 7 finder pattern with a 1-module-wide white separator.
    /// `(top, left)` is the top-left corner of the 7×7 finder.
    fn draw_finder(&mut self, top: usize, left: usize) {
        let size = self.size as usize;
        // The finder pattern itself
        for r in 0..7usize {
            for c in 0..7usize {
                let dark = r == 0 || r == 6 || c == 0 || c == 6
                    || (r >= 2 && r <= 4 && c >= 2 && c <= 4);
                self.set_func(top + r, left + c, dark);
            }
        }
        // Separator: white border 1 module wide around the finder pattern,
        // clamped to the grid boundary.
        for r in 0..=7usize {
            for c in 0..=7usize {
                // Only the border cells of the 8×8 region
                if r == 7 || c == 7 {
                    let row = top + r;
                    let col = left + c;
                    if row < size && col < size {
                        self.set_func(row, col, false);
                    }
                }
            }
        }
    }

    /// Check if `(row, col)` falls inside one of the three finder-pattern regions.
    fn is_finder_region(&self, row: usize, col: usize) -> bool {
        let s = self.size as usize;
        (row < 9 && col < 9) || (row < 9 && col >= s - 8) || (row >= s - 8 && col < 9)
    }

    /// Draw a 5 × 5 alignment pattern centred at `(cr, cc)`.
    fn draw_alignment(&mut self, cr: usize, cc: usize) {
        for dr in -2isize..=2 {
            for dc in -2isize..=2 {
                let r = (cr as isize + dr) as usize;
                let c = (cc as isize + dc) as usize;
                let dark = dr.abs() == 2 || dc.abs() == 2 || (dr == 0 && dc == 0);
                self.set_func(r, c, dark);
            }
        }
    }

    /// Mark the two format-information strips as function modules (value TBD).
    fn reserve_format_areas(&mut self) {
        let size = self.size as usize;
        // Horizontal strip along row 8 (near top-left finder)
        for col in 0..=8usize {
            if col != 6 {
                // col 6 is already the timing pattern
                let cur = self.get(8, col);
                self.set(8, col, cur | FUNC);
            }
        }
        // Vertical strip along col 8 (near top-left finder)
        for row in 0..=8usize {
            if row != 6 {
                let cur = self.get(row, 8);
                self.set(row, 8, cur | FUNC);
            }
        }
        // Second copy — bottom-left & top-right strips
        for i in 0..8usize {
            let cur = self.get(size - 1 - i, 8);
            self.set(size - 1 - i, 8, cur | FUNC);
        }
        for i in 0..8usize {
            let cur = self.get(8, size - 8 + i);
            self.set(8, size - 8 + i, cur | FUNC);
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Codeword placement (zigzag pattern)
    // ─────────────────────────────────────────────────────────────────────────

    fn place_codewords(&mut self, codewords: &[u8]) {
        let size = self.size as usize;
        let mut cw_idx = 0usize;
        let mut bit_idx = 7i32;
        let mut upward = true;

        // Iterate over vertical bands of width 2, right-to-left, skipping col 6.
        let mut col = size as isize - 1;
        while col >= 0 {
            if col == 6 {
                col -= 1; // skip timing-pattern column
                continue;
            }

            for row_offset in 0..size {
                let row = if upward { size - 1 - row_offset } else { row_offset };
                for dc in 0usize..2 {
                    let c = (col - dc as isize) as usize;
                    if c < size {
                        let m = self.get(row, c);
                        if m & FUNC == 0 {
                            // Place the next codeword bit here.
                            let bit = if cw_idx < codewords.len() {
                                (codewords[cw_idx] >> bit_idx) & 1
                            } else {
                                0
                            };
                            self.set(row, c, if bit != 0 { DARK } else { 0 });
                            bit_idx -= 1;
                            if bit_idx < 0 {
                                cw_idx += 1;
                                bit_idx = 7;
                            }
                        }
                    }
                }
            }
            upward = !upward;
            col -= 2;
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Masking
    // ─────────────────────────────────────────────────────────────────────────

    fn mask_bit(mask: u8, row: usize, col: usize) -> bool {
        let (r, c) = (row as i32, col as i32);
        match mask {
            0 => (r + c) % 2 == 0,
            1 => r % 2 == 0,
            2 => c % 3 == 0,
            3 => (r + c) % 3 == 0,
            4 => (r / 2 + c / 3) % 2 == 0,
            5 => (r * c) % 2 + (r * c) % 3 == 0,
            6 => ((r * c) % 2 + (r * c) % 3) % 2 == 0,
            7 => ((r + c) % 2 + (r * c) % 3) % 2 == 0,
            _ => false,
        }
    }

    fn apply_mask(&mut self, mask: u8) {
        let size = self.size as usize;
        for row in 0..size {
            for col in 0..size {
                let m = self.get(row, col);
                if m & FUNC == 0 && Self::mask_bit(mask, row, col) {
                    self.set(row, col, m ^ DARK);
                }
            }
        }
    }

    fn penalty_score(&self, mask: u8) -> u32 {
        let size = self.size as usize;
        // Temporarily apply the mask to compute the score, then undo it.
        // We work on a temporary copy to avoid mutating `self`.
        let mut tmp = [0u8; MAX_DIM * MAX_DIM];
        tmp[..size * MAX_DIM].copy_from_slice(&self.grid[..size * MAX_DIM]);

        for row in 0..size {
            for col in 0..size {
                let idx = row * MAX_DIM + col;
                let m = tmp[idx];
                if m & FUNC == 0 && Self::mask_bit(mask, row, col) {
                    tmp[idx] = m ^ DARK;
                }
            }
        }

        let is_dark = |r: usize, c: usize| -> bool { tmp[r * MAX_DIM + c] & DARK != 0 };

        let mut penalty = 0u32;

        // Rule 1: five or more consecutive same-color modules in a row/column.
        for row in 0..size {
            let mut run_dark = 0u32;
            let mut run_light = 0u32;
            for col in 0..size {
                if is_dark(row, col) {
                    run_dark += 1;
                    run_light = 0;
                } else {
                    run_light += 1;
                    run_dark = 0;
                }
                if run_dark == 5 || run_light == 5 {
                    penalty += 3;
                } else if run_dark > 5 || run_light > 5 {
                    penalty += 1;
                }
            }
        }
        for col in 0..size {
            let mut run_dark = 0u32;
            let mut run_light = 0u32;
            for row in 0..size {
                if is_dark(row, col) {
                    run_dark += 1;
                    run_light = 0;
                } else {
                    run_light += 1;
                    run_dark = 0;
                }
                if run_dark == 5 || run_light == 5 {
                    penalty += 3;
                } else if run_dark > 5 || run_light > 5 {
                    penalty += 1;
                }
            }
        }

        // Rule 2: 2×2 blocks of same color.
        for row in 0..size - 1 {
            for col in 0..size - 1 {
                let tl = is_dark(row, col);
                if tl == is_dark(row, col + 1)
                    && tl == is_dark(row + 1, col)
                    && tl == is_dark(row + 1, col + 1)
                {
                    penalty += 3;
                }
            }
        }

        // Rule 3: finder-like patterns.
        const PATTERN_A: [bool; 11] = [
            true, false, true, true, true, false, true, false, false, false, false,
        ];
        const PATTERN_B: [bool; 11] = [
            false, false, false, false, true, false, true, true, true, false, true,
        ];
        if size >= 11 {
            for row in 0..size {
                for col in 0..size - 10 {
                    let mut seq = [false; 11];
                    for k in 0..11 {
                        seq[k] = is_dark(row, col + k);
                    }
                    if seq == PATTERN_A || seq == PATTERN_B {
                        penalty += 40;
                    }
                }
            }
            for col in 0..size {
                for row in 0..size - 10 {
                    let mut seq = [false; 11];
                    for k in 0..11 {
                        seq[k] = is_dark(row + k, col);
                    }
                    if seq == PATTERN_A || seq == PATTERN_B {
                        penalty += 40;
                    }
                }
            }
        }

        // Rule 4: proportion of dark modules.
        let total = (size * size) as u32;
        let mut dark_count = 0u32;
        for r in 0..size {
            for c in 0..size {
                if is_dark(r, c) {
                    dark_count += 1;
                }
            }
        }
        let percent = dark_count * 100 / total;
        let prev5 = (percent / 5) * 5;
        let next5 = prev5 + 5;
        let dev_prev = if prev5 >= 50 { prev5 - 50 } else { 50 - prev5 };
        let dev_next = if next5 >= 50 { next5 - 50 } else { 50 - next5 };
        penalty += (dev_prev.min(dev_next) / 5) * 10;

        penalty
    }

    fn choose_mask(&self) -> u8 {
        let mut best_mask = 0u8;
        let mut best_penalty = u32::MAX;
        for mask in 0u8..8 {
            let p = self.penalty_score(mask);
            if p < best_penalty {
                best_penalty = p;
                best_mask = mask;
            }
        }
        best_mask
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Format information
    // ─────────────────────────────────────────────────────────────────────────

    fn draw_format_bits(&mut self, mask: u8) {
        let size = self.size as usize;
        let bits = FORMAT_BITS_ECC_M[mask as usize];
        let bit_at = |n: u16| -> bool { (bits >> n) & 1 != 0 };

        // Helper: write a single format-info module.
        macro_rules! sfmt {
            ($r:expr, $c:expr, $b:expr) => {
                self.set($r, $c, FUNC | if $b { DARK } else { 0 })
            };
        }

        // ── First copy ────────────────────────────────────────────────────────
        // Row 8, columns 0-5 (bits 0-5), then skip col 6 (timing), then 7-8
        sfmt!(8, 0, bit_at(0));
        sfmt!(8, 1, bit_at(1));
        sfmt!(8, 2, bit_at(2));
        sfmt!(8, 3, bit_at(3));
        sfmt!(8, 4, bit_at(4));
        sfmt!(8, 5, bit_at(5));
        // col 6 = timing pattern — already FUNC; do not overwrite
        sfmt!(8, 7, bit_at(6));
        sfmt!(8, 8, bit_at(7));

        // Col 8, rows 7 down to 0 (skip row 6 = timing)
        sfmt!(7, 8, bit_at(8));
        // row 6 = timing pattern — skip
        sfmt!(5, 8, bit_at(9));
        sfmt!(4, 8, bit_at(10));
        sfmt!(3, 8, bit_at(11));
        sfmt!(2, 8, bit_at(12));
        sfmt!(1, 8, bit_at(13));
        sfmt!(0, 8, bit_at(14));

        // ── Second copy ───────────────────────────────────────────────────────
        // Col 8, bottom-left finder region: bits 0-7 from bottom upward
        for i in 0..8usize {
            sfmt!(size - 1 - i, 8, bit_at(i as u16));
        }
        // Row 8, top-right finder region: bits 8-14 from left to right
        for i in 8..=14usize {
            sfmt!(8, size - 15 + i, bit_at(i as u16));
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Unit tests (run on host with `cargo test --lib`)
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gf_mul_zero() {
        let gf = Gf::new();
        assert_eq!(gf.mul(0, 255), 0);
        assert_eq!(gf.mul(1, 0), 0);
    }

    #[test]
    fn gf_mul_one() {
        let gf = Gf::new();
        for x in 1u8..=255 {
            assert_eq!(gf.mul(x, 1), x, "x * 1 != x for x={x}");
        }
    }

    #[test]
    fn gf_mul_two_matches_shift_xor() {
        // Multiplying by 2 (α) in GF(256) with poly 0x11D is a left shift + XOR.
        let gf = Gf::new();
        for x in 1u8..=255 {
            let expected = if x & 0x80 != 0 {
                ((x as u16) << 1 ^ 0x11D) as u8
            } else {
                x << 1
            };
            assert_eq!(gf.mul(x, 2), expected, "gf_mul({x}, 2)");
        }
    }

    #[test]
    fn version_selection() {
        assert_eq!(QrCode::select_version(0), Some(1));
        assert_eq!(QrCode::select_version(14), Some(1));
        assert_eq!(QrCode::select_version(15), Some(2));
        assert_eq!(QrCode::select_version(26), Some(2));
        assert_eq!(QrCode::select_version(84), Some(5));
        assert_eq!(QrCode::select_version(85), None);
    }

    #[test]
    fn encode_grocycode() {
        let qr = QrCode::encode(b"grcy-p-1").expect("encode failed");
        assert_eq!(qr.version(), 1);
        assert_eq!(qr.size(), 21);
    }

    #[test]
    fn encode_url() {
        // Typical Grocy product URL
        let qr = QrCode::encode(b"http://grocy.local/api/stock/5").expect("encode failed");
        assert!(qr.version() >= 2);
        assert_eq!(qr.size(), (17 + 4 * qr.version()) as usize);
    }

    #[test]
    fn module_bounds() {
        let qr = QrCode::encode(b"test").unwrap();
        let s = qr.size();
        // Corner modules — just check they don't panic.
        let _ = qr.module(0, 0);
        let _ = qr.module(s - 1, s - 1);
    }

    #[test]
    fn finder_top_left_corner_is_dark() {
        let qr = QrCode::encode(b"hello").unwrap();
        // Top-left corner of every QR code is always dark (black).
        assert!(qr.module(0, 0));
    }
}
