// Tiny QR code encoder (byte mode, error correction level M, versions 1-10:
// up to 213 bytes - plenty for a URL) so `monctl cast` can show a code the
// tablet's camera opens directly. Follows ISO/IEC 18004; structure after
// Nayuki's reference implementation.

/// (data codewords per block for each block, EC codewords per block)
fn blocks(version: usize) -> (Vec<usize>, usize) {
    let (g1, d1, g2, d2, ec) = match version {
        1 => (1, 16, 0, 0, 10),
        2 => (1, 28, 0, 0, 16),
        3 => (1, 44, 0, 0, 26),
        4 => (2, 32, 0, 0, 18),
        5 => (2, 43, 0, 0, 24),
        6 => (4, 27, 0, 0, 16),
        7 => (4, 31, 0, 0, 18),
        8 => (2, 38, 2, 39, 22),
        9 => (3, 36, 2, 37, 22),
        _ => (4, 43, 1, 44, 26),
    };
    let mut v = vec![d1; g1];
    v.extend(std::iter::repeat_n(d2, g2));
    (v, ec)
}

fn alignment(version: usize) -> &'static [usize] {
    match version {
        1 => &[],
        2 => &[6, 18],
        3 => &[6, 22],
        4 => &[6, 26],
        5 => &[6, 30],
        6 => &[6, 34],
        7 => &[6, 22, 38],
        8 => &[6, 24, 42],
        9 => &[6, 26, 46],
        _ => &[6, 28, 50],
    }
}

fn gf_mul(x: u8, y: u8) -> u8 {
    let mut z: u32 = 0;
    for i in (0..8).rev() {
        z = (z << 1) ^ ((z >> 7) * 0x11D);
        z ^= ((y as u32 >> i) & 1) * x as u32;
    }
    z as u8
}

fn rs_ecc(data: &[u8], degree: usize) -> Vec<u8> {
    let mut divisor = vec![0u8; degree];
    divisor[degree - 1] = 1;
    let mut root = 1u8;
    for _ in 0..degree {
        for j in 0..degree {
            divisor[j] = gf_mul(divisor[j], root);
            if j + 1 < degree {
                divisor[j] ^= divisor[j + 1];
            }
        }
        root = gf_mul(root, 2);
    }
    let mut result = vec![0u8; degree];
    for &b in data {
        let factor = b ^ result.remove(0);
        result.push(0);
        for (r, d) in result.iter_mut().zip(&divisor) {
            *r ^= gf_mul(*d, factor);
        }
    }
    result
}

struct Grid {
    size: usize,
    dark: Vec<bool>,
    function: Vec<bool>,
}

impl Grid {
    fn set(&mut self, x: usize, y: usize, dark: bool) {
        self.dark[y * self.size + x] = dark;
        self.function[y * self.size + x] = true;
    }
    fn get(&self, x: usize, y: usize) -> bool {
        self.dark[y * self.size + x]
    }
}

fn format_bits(mask: usize) -> u32 {
    let data = mask as u32; // level M = 0b00
    let mut rem = data;
    for _ in 0..10 {
        rem = (rem << 1) ^ ((rem >> 9) * 0x537);
    }
    ((data << 10) | rem) ^ 0x5412
}

fn draw_format(g: &mut Grid, mask: usize) {
    let bits = format_bits(mask);
    let bit = |i: u32| (bits >> i) & 1 != 0;
    let s = g.size;
    for i in 0..=5 {
        g.set(8, i, bit(i as u32));
    }
    g.set(8, 7, bit(6));
    g.set(8, 8, bit(7));
    g.set(7, 8, bit(8));
    for i in 9..15 {
        g.set(14 - i, 8, bit(i as u32));
    }
    for i in 0..8 {
        g.set(s - 1 - i, 8, bit(i as u32));
    }
    for i in 8..15 {
        g.set(8, s - 15 + i, bit(i as u32));
    }
    g.set(8, s - 8, true);
}

fn function_patterns(version: usize) -> Grid {
    let size = version * 4 + 17;
    let mut g = Grid { size, dark: vec![false; size * size], function: vec![false; size * size] };
    for i in 0..size {
        g.set(6, i, i % 2 == 0);
        g.set(i, 6, i % 2 == 0);
    }
    for (cx, cy) in [(3, 3), (size - 4, 3), (3, size - 4)] {
        for dy in -4i32..=4 {
            for dx in -4i32..=4 {
                let (x, y) = (cx as i32 + dx, cy as i32 + dy);
                if (0..size as i32).contains(&x) && (0..size as i32).contains(&y) {
                    let d = dx.abs().max(dy.abs());
                    g.set(x as usize, y as usize, d != 2 && d != 4);
                }
            }
        }
    }
    let al = alignment(version);
    for (i, &ax) in al.iter().enumerate() {
        for (j, &ay) in al.iter().enumerate() {
            let last = al.len() - 1;
            if (i == 0 && (j == 0 || j == last)) || (i == last && j == 0) {
                continue; // overlaps a finder
            }
            for dy in -2i32..=2 {
                for dx in -2i32..=2 {
                    g.set((ax as i32 + dx) as usize, (ay as i32 + dy) as usize, dx.abs().max(dy.abs()) != 1);
                }
            }
        }
    }
    draw_format(&mut g, 0); // reserve; real bits drawn after masking
    if version >= 7 {
        let mut rem = version as u32;
        for _ in 0..12 {
            rem = (rem << 1) ^ ((rem >> 11) * 0x1F25);
        }
        let bits = (version as u32) << 12 | rem;
        for i in 0..18 {
            let b = (bits >> i) & 1 != 0;
            let (a, c) = (size - 11 + i % 3, i / 3);
            g.set(a, c, b);
            g.set(c, a, b);
        }
    }
    g
}

// written exactly as the spec's mask table, hence no is_multiple_of()
#[allow(clippy::manual_is_multiple_of)]
fn masked(mask: usize, x: usize, y: usize) -> bool {
    match mask {
        0 => (x + y) % 2 == 0,
        1 => y % 2 == 0,
        2 => x % 3 == 0,
        3 => (x + y) % 3 == 0,
        4 => (x / 3 + y / 2) % 2 == 0,
        5 => x * y % 2 + x * y % 3 == 0,
        6 => (x * y % 2 + x * y % 3) % 2 == 0,
        _ => ((x + y) % 2 + x * y % 3) % 2 == 0,
    }
}

fn penalty(g: &Grid) -> usize {
    let s = g.size;
    let mut p = 0;
    // runs of 5+ same-colour modules, rows and columns
    for horizontal in [true, false] {
        for a in 0..s {
            let mut run = 0;
            let mut last = None;
            for b in 0..s {
                let v = if horizontal { g.get(b, a) } else { g.get(a, b) };
                if Some(v) == last {
                    run += 1;
                } else {
                    if run >= 5 {
                        p += run - 2;
                    }
                    run = 1;
                    last = Some(v);
                }
            }
            if run >= 5 {
                p += run - 2;
            }
        }
    }
    // 2x2 blocks
    for y in 0..s - 1 {
        for x in 0..s - 1 {
            let v = g.get(x, y);
            if v == g.get(x + 1, y) && v == g.get(x, y + 1) && v == g.get(x + 1, y + 1) {
                p += 3;
            }
        }
    }
    // finder-like 1:1:3:1:1 with a light margin
    const PAT: [bool; 11] = [true, false, true, true, true, false, true, false, false, false, false];
    for horizontal in [true, false] {
        for a in 0..s {
            for b in 0..=s.saturating_sub(11) {
                let at = |k: usize| if horizontal { g.get(b + k, a) } else { g.get(a, b + k) };
                if (0..11).all(|k| at(k) == PAT[k]) || (0..11).all(|k| at(k) == PAT[10 - k]) {
                    p += 40;
                }
            }
        }
    }
    // dark/light balance
    let dark = g.dark.iter().filter(|&&d| d).count();
    let pct = dark * 100 / (s * s);
    p + (pct.abs_diff(50) / 5) * 10
}

/// Module matrix (true = dark) for `text`, or None if it doesn't fit.
pub fn encode(text: &str) -> Option<Vec<Vec<bool>>> {
    let bytes = text.as_bytes();
    let version = (1..=10).find(|&v| {
        let (b, _) = blocks(v);
        let cap = b.iter().sum::<usize>() * 8;
        4 + if v < 10 { 8 } else { 16 } + bytes.len() * 8 <= cap
    })?;
    let (layout, ec) = blocks(version);
    let capacity = layout.iter().sum::<usize>();

    // bit stream: mode 0100, length, bytes, terminator, pad
    let mut bits: Vec<bool> = vec![];
    let mut push = |v: u32, n: usize| (0..n).rev().for_each(|i| bits.push((v >> i) & 1 != 0));
    push(0b0100, 4);
    push(bytes.len() as u32, if version < 10 { 8 } else { 16 });
    for &b in bytes {
        push(b as u32, 8);
    }
    let room = capacity * 8 - bits.len();
    bits.extend(std::iter::repeat_n(false, room.min(4)));
    while !bits.len().is_multiple_of(8) {
        bits.push(false);
    }
    let mut data: Vec<u8> = bits.chunks(8).map(|c| c.iter().fold(0u8, |a, &b| a << 1 | b as u8)).collect();
    let mut pad = [0xEC, 0x11].into_iter().cycle();
    while data.len() < capacity {
        data.push(pad.next().unwrap());
    }

    // split into blocks, add EC, interleave
    let mut chunks = vec![];
    let mut off = 0;
    for n in &layout {
        chunks.push(data[off..off + n].to_vec());
        off += n;
    }
    let eccs: Vec<Vec<u8>> = chunks.iter().map(|c| rs_ecc(c, ec)).collect();
    let mut all = vec![];
    for i in 0..*layout.iter().max().unwrap() {
        for c in &chunks {
            if let Some(&b) = c.get(i) {
                all.push(b);
            }
        }
    }
    for i in 0..ec {
        for e in &eccs {
            all.push(e[i]);
        }
    }

    // zigzag placement
    let mut g = function_patterns(version);
    let s = g.size;
    let mut i = 0;
    let mut right = s as i32 - 1;
    while right >= 1 {
        if right == 6 {
            right = 5;
        }
        for vert in 0..s {
            for j in 0..2 {
                let x = right as usize - j;
                let upward = (right + 1) & 2 == 0;
                let y = if upward { s - 1 - vert } else { vert };
                if !g.function[y * s + x] && i < all.len() * 8 {
                    g.dark[y * s + x] = (all[i >> 3] >> (7 - (i & 7))) & 1 != 0;
                    i += 1;
                }
            }
        }
        right -= 2;
    }

    // pick the mask with the lowest penalty
    let apply = |g: &mut Grid, mask: usize| {
        for y in 0..s {
            for x in 0..s {
                if !g.function[y * s + x] && masked(mask, x, y) {
                    g.dark[y * s + x] ^= true;
                }
            }
        }
    };
    let best = (0..8)
        .min_by_key(|&m| {
            let mut t = Grid { size: s, dark: g.dark.clone(), function: g.function.clone() };
            apply(&mut t, m);
            draw_format(&mut t, m);
            penalty(&t)
        })
        .unwrap();
    apply(&mut g, best);
    draw_format(&mut g, best);
    Some((0..s).map(|y| (0..s).map(|x| g.get(x, y)).collect()).collect())
}

/// The code drawn with half blocks (two module rows per text line), black on
/// white with a quiet zone, explicit colours so dark terminal themes don't
/// invert it into something cameras won't read.
pub fn terminal(text: &str) -> String {
    let Some(m) = encode(text) else { return String::new() };
    let quiet = 2;
    let n = m.len() + quiet * 2;
    let dark = |x: usize, y: usize| -> bool {
        x >= quiet && y >= quiet && x - quiet < m.len() && y - quiet < m.len() && m[y - quiet][x - quiet]
    };
    let mut out = String::new();
    for y in (0..n).step_by(2) {
        out.push_str("  ");
        for x in 0..n {
            let (top, bottom) = (dark(x, y), y + 1 < n && dark(x, y + 1));
            // foreground paints the upper half (▀), background the lower
            out.push_str(&format!("\x1b[{};{}m▀", if top { 30 } else { 97 }, if bottom { 40 } else { 107 }));
        }
        out.push_str("\x1b[0m\n");
    }
    out
}

/// The code as an SVG (black on white, 4-module quiet zone) for pages.
pub fn svg(text: &str) -> String {
    let Some(m) = encode(text) else { return String::new() };
    let n = m.len() + 8;
    let mut path = String::new();
    for (y, row) in m.iter().enumerate() {
        for (x, &dark) in row.iter().enumerate() {
            if dark {
                path.push_str(&format!("M{} {}h1v1h-1z", x + 4, y + 4));
            }
        }
    }
    format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {n} {n}\" shape-rendering=\"crispEdges\"><rect width=\"{n}\" height=\"{n}\" fill=\"#fff\"/><path d=\"{path}\" fill=\"#000\"/></svg>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_ecc() {
        // "HELLO WORLD" 1-M data codewords and their EC (Thonky's worked example)
        let data = [32, 91, 11, 120, 209, 114, 220, 77, 67, 64, 236, 17, 236, 17, 236, 17];
        assert_eq!(rs_ecc(&data, 10), vec![196, 35, 39, 119, 235, 215, 231, 226, 93, 23]);
    }

    #[test]
    fn sizes_and_finders() {
        let m = encode("http://192.168.100.200:8484/").unwrap();
        assert_eq!(m.len(), 29); // version 3
        // finder corners: dark 7x7 ring with light separator
        for (x, y) in [(0, 0), (m.len() - 7, 0), (0, m.len() - 7)] {
            assert!(m[y][x] && m[y + 6][x + 6] && !m[y + 1][x + 1] && m[y + 3][x + 3]);
        }
        assert!(encode(&"x".repeat(213)).is_some());
        assert!(encode(&"x".repeat(214)).is_none());
    }

    #[test]
    fn format_bits_match_the_spec_table() {
        // level M, mask 0..7 (ISO 18004 table C.1)
        let want = [0x5412, 0x5125, 0x5E7C, 0x5B4B, 0x45F9, 0x40CE, 0x4F97, 0x4AA0];
        for (m, w) in want.iter().enumerate() {
            assert_eq!(format_bits(m), *w, "mask {m}");
        }
    }
}
