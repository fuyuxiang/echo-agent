//! Procedural art assets for the `/gboom` easter egg.
//!
//! Everything is generated in code — no binary assets, no copyrighted
//! material. Wall textures are synthesized from hash noise, sprites are
//! hand-drawn char-map pixel art, and text uses a tiny 5x7 pixel font.

/// RGB color.
pub(super) type Rgb = [u8; 3];

/// Wall texture side length (square).
pub(super) const TEX_SIZE: usize = 64;

/// The signature crimson red, shared by the title screen, end screens,
/// and the overlay chrome so they always match.
pub(crate) const GBOOM_RED: Rgb = [235, 40, 32];

/// Imp eye color. The renderer exempts exactly this color from distance
/// fog so eyes glow in the dark; keep the sprite art and renderer in sync
/// through this constant.
pub(super) const EYE_GLOW: Rgb = [255, 216, 0];

/// Minimal xorshift64* PRNG.
///
/// The game needs determinism-friendly, allocation-free randomness for
/// damage rolls and the fire effect — not `rand`-crate quality. Seeded
/// per consumer so simulation and visuals stay independent streams.
pub(super) struct XorShift64(u64);

impl XorShift64 {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1)) // xorshift state must be non-zero
    }

    pub fn next_u32(&mut self) -> u32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545F4914F6CDD1D) >> 33) as u32
    }

    /// Uniform float in `[0.0, 1.0)`.
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }
}

/// Deterministic 2D integer hash → `[0.0, 1.0)`. Used for texture noise.
fn hash01(x: u32, y: u32, seed: u32) -> f32 {
    let mut h = x
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(y.wrapping_mul(0x85EB_CA6B))
        .wrapping_add(seed.wrapping_mul(0xC2B2_AE35));
    h ^= h >> 16;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846C_A68B);
    h ^= h >> 16;
    (h & 0xFFFF) as f32 / 65536.0
}

fn scale(c: Rgb, f: f32) -> Rgb {
    [
        (c[0] as f32 * f).clamp(0.0, 255.0) as u8,
        (c[1] as f32 * f).clamp(0.0, 255.0) as u8,
        (c[2] as f32 * f).clamp(0.0, 255.0) as u8,
    ]
}

/// A generated wall texture: `TEX_SIZE * TEX_SIZE` RGB pixels, row-major.
pub(super) struct Texture {
    pub pixels: Vec<Rgb>,
}

impl Texture {
    #[inline]
    pub fn sample(&self, x: usize, y: usize) -> Rgb {
        self.pixels[(y & (TEX_SIZE - 1)) * TEX_SIZE + (x & (TEX_SIZE - 1))]
    }
}

/// Generate the wall texture set, indexed by map cell value - 1.
pub(super) fn build_textures() -> Vec<Texture> {
    vec![brick(), stone(), tech(), hellstone()]
}

/// Classic red-brown brick: 16x8 bricks, offset every other row, mortar gaps.
fn brick() -> Texture {
    let base: Rgb = [148, 64, 44];
    let mortar: Rgb = [78, 60, 54];
    let mut pixels = Vec::with_capacity(TEX_SIZE * TEX_SIZE);
    for y in 0..TEX_SIZE {
        for x in 0..TEX_SIZE {
            let row = y / 8;
            let off = if row % 2 == 0 { 0 } else { 8 };
            let in_mortar = y % 8 == 0 || (x + off) % 16 == 0;
            let c = if in_mortar {
                mortar
            } else {
                // Per-brick tone variation + per-pixel grain.
                let brick_id = (row as u32) * 31 + (((x + off) / 16) as u32);
                let tone = 0.82 + 0.30 * hash01(brick_id, 7, 1);
                let grain = 0.92 + 0.16 * hash01(x as u32, y as u32, 2);
                scale(base, tone * grain)
            };
            pixels.push(c);
        }
    }
    Texture { pixels }
}

/// Large gray stone blocks with grout lines and noise.
fn stone() -> Texture {
    let base: Rgb = [118, 118, 126];
    let grout: Rgb = [58, 58, 64];
    let mut pixels = Vec::with_capacity(TEX_SIZE * TEX_SIZE);
    for y in 0..TEX_SIZE {
        for x in 0..TEX_SIZE {
            let row = y / 16;
            let off = if row % 2 == 0 { 0 } else { 16 };
            let in_grout = y % 16 == 0 || (x + off) % 32 == 0;
            let c = if in_grout {
                grout
            } else {
                let block_id = (row as u32) * 17 + (((x + off) / 32) as u32);
                let tone = 0.80 + 0.28 * hash01(block_id, 3, 3);
                let grain = 0.90 + 0.20 * hash01(x as u32, y as u32, 4);
                scale(base, tone * grain)
            };
            pixels.push(c);
        }
    }
    Texture { pixels }
}

/// Dark sci-fi metal panels with horizontal seams and green light strips.
fn tech() -> Texture {
    let base: Rgb = [74, 82, 92];
    let seam: Rgb = [38, 42, 48];
    let light: Rgb = [110, 240, 130];
    let mut pixels = Vec::with_capacity(TEX_SIZE * TEX_SIZE);
    for y in 0..TEX_SIZE {
        for x in 0..TEX_SIZE {
            let in_seam = y % 16 == 0 || y % 16 == 15 || x % 32 == 0;
            // Blinking-looking light dots along the middle of each panel.
            let is_light =
                y % 16 == 8 && x % 8 == 4 && hash01((x / 8) as u32, (y / 16) as u32, 5) > 0.35;
            let c = if is_light {
                light
            } else if in_seam {
                seam
            } else {
                let grain = 0.88 + 0.22 * hash01(x as u32, y as u32, 6);
                scale(base, grain)
            };
            pixels.push(c);
        }
    }
    Texture { pixels }
}

/// Floor: large worn stone tiles with grime patches, kept dark so the
/// fog gradient toward the horizon reads naturally.
pub(super) fn build_floor_texture() -> Texture {
    let base: Rgb = [96, 86, 74];
    let grout: Rgb = [44, 40, 36];
    let mut pixels = Vec::with_capacity(TEX_SIZE * TEX_SIZE);
    for y in 0..TEX_SIZE {
        for x in 0..TEX_SIZE {
            let in_grout = y % 32 == 0 || x % 32 == 0;
            let c = if in_grout {
                grout
            } else {
                let tile_id = (y / 32) as u32 * 5 + (x / 32) as u32;
                let tone = 0.78 + 0.30 * hash01(tile_id, 11, 9);
                // Grime: coarse blotches darken patches of the tile.
                let grime = if hash01((x / 6) as u32, (y / 6) as u32, 10) > 0.72 {
                    0.78
                } else {
                    1.0
                };
                let grain = 0.90 + 0.20 * hash01(x as u32, y as u32, 11);
                scale(base, tone * grime * grain)
            };
            pixels.push(c);
        }
    }
    Texture { pixels }
}

/// Ceiling: dark metal panels with sparse emissive light squares. The
/// lights stay bright through fog shading simply by starting near white.
pub(super) fn build_ceiling_texture() -> Texture {
    let base: Rgb = [52, 56, 66];
    let seam: Rgb = [30, 32, 38];
    let lamp: Rgb = [232, 226, 198];
    let mut pixels = Vec::with_capacity(TEX_SIZE * TEX_SIZE);
    for y in 0..TEX_SIZE {
        for x in 0..TEX_SIZE {
            let in_seam = y % 16 == 0 || x % 16 == 0;
            // One panel in ~6 carries a recessed lamp in its center.
            let panel = ((x / 16) as u32, (y / 16) as u32);
            let has_lamp = hash01(panel.0, panel.1, 12) > 0.84;
            let in_lamp = has_lamp && (4..12).contains(&(x % 16)) && (4..12).contains(&(y % 16));
            let c = if in_lamp {
                lamp
            } else if in_seam {
                seam
            } else {
                let grain = 0.88 + 0.20 * hash01(x as u32, y as u32, 13);
                scale(base, grain)
            };
            pixels.push(c);
        }
    }
    Texture { pixels }
}

/// Dark red marbled "hell" stone for accent walls.
fn hellstone() -> Texture {
    let base: Rgb = [120, 30, 28];
    let vein: Rgb = [200, 80, 50];
    let mut pixels = Vec::with_capacity(TEX_SIZE * TEX_SIZE);
    for y in 0..TEX_SIZE {
        for x in 0..TEX_SIZE {
            let fx = x as f32 / TEX_SIZE as f32;
            let fy = y as f32 / TEX_SIZE as f32;
            // Cheap marble: layered sine waves distorted by noise.
            let n = hash01((x / 4) as u32, (y / 4) as u32, 7);
            let v = ((fx * 9.0 + fy * 4.0 + n * 3.0).sin() * 0.5 + 0.5).powi(3);
            let grain = 0.85 + 0.25 * hash01(x as u32, y as u32, 8);
            let c = [
                (base[0] as f32 * (1.0 - v) + vein[0] as f32 * v) * grain,
                (base[1] as f32 * (1.0 - v) + vein[1] as f32 * v) * grain,
                (base[2] as f32 * (1.0 - v) + vein[2] as f32 * v) * grain,
            ];
            pixels.push([c[0] as u8, c[1] as u8, c[2] as u8]);
        }
    }
    Texture { pixels }
}

// -------------------------------------------------------------------------
// Sprite art (char-map pixel art)
// -------------------------------------------------------------------------

/// Map a sprite art character to a color. `.` is transparent.
fn sprite_color(ch: u8) -> Option<Rgb> {
    match ch {
        b'.' => None,
        b'B' => Some([146, 90, 50]),   // imp body, brown
        b'b' => Some([104, 62, 34]),   // imp body, shaded
        b'H' => Some([222, 214, 188]), // horn / bone
        b'E' => Some(EYE_GLOW),        // glowing eye (fog-exempt in renderer)
        b'M' => Some([34, 20, 16]),    // mouth / dark recess
        b'T' => Some([236, 232, 220]), // teeth
        b'C' => Some([214, 196, 160]), // claw
        b'R' => Some([186, 28, 24]),   // blood
        b'r' => Some([120, 16, 14]),   // blood, dark
        b'G' => Some([96, 104, 112]),  // gunmetal
        b'g' => Some([52, 58, 66]),    // gunmetal, dark
        b'W' => Some([224, 228, 232]), // highlight
        b'S' => Some([212, 160, 116]), // skin
        b's' => Some([164, 116, 80]),  // skin, shaded
        b'F' => Some([255, 244, 160]), // muzzle flash core
        b'f' => Some([255, 168, 48]),  // muzzle flash fringe
        _ => None,
    }
}

/// A char-map sprite: rows of equal length, `.` = transparent.
pub(super) struct Sprite {
    pub w: usize,
    pub h: usize,
    pixels: Vec<Option<Rgb>>,
}

impl Sprite {
    fn from_art(art: &[&str]) -> Self {
        let h = art.len();
        let w = art.first().map_or(0, |r| r.len());
        let mut pixels = Vec::with_capacity(w * h);
        for row in art {
            debug_assert_eq!(row.len(), w, "sprite rows must be equal length");
            for &ch in row.as_bytes() {
                pixels.push(sprite_color(ch));
            }
        }
        Self { w, h, pixels }
    }

    /// Sample with normalized coordinates in `[0, 1)`.
    #[inline]
    pub fn sample(&self, u: f32, v: f32) -> Option<Rgb> {
        let x = ((u * self.w as f32) as usize).min(self.w - 1);
        let y = ((v * self.h as f32) as usize).min(self.h - 1);
        self.pixels[y * self.w + x]
    }
}

/// Imp frame set, indexed by [`super::game::ImpVisual`].
pub(super) struct ImpSprites {
    pub walk_a: Sprite,
    pub walk_b: Sprite,
    pub attack: Sprite,
    pub pain: Sprite,
    pub die_a: Sprite,
    pub die_b: Sprite,
    pub corpse: Sprite,
}

pub(super) fn build_imp_sprites() -> ImpSprites {
    // 16x20 horned demon. Two walk frames differ in leg/arm pose.
    let walk_a = Sprite::from_art(&[
        "..H..........H..",
        "..HH........HH..",
        "...bBBBBBBBBb...",
        "...BBBBBBBBBB...",
        "..BBEEBBBBEEBB..",
        "..BBEEBBBBEEBB..",
        "...BBBBBBBBBB...",
        "...BbMTMTMTbB...",
        "....bBBBBBBb....",
        "..bBBBBBBBBBBb..",
        ".CBBb.BBBB.bBBC.",
        ".CBB..BBBB..BBC.",
        ".CC...BBBB...CC.",
        "......bBBb......",
        ".....BB..BB.....",
        "....BB....BB....",
        "....BB.....BB...",
        "...bB.......Bb..",
        "...BB.......BB..",
        "..CC.........CC.",
    ]);
    let walk_b = Sprite::from_art(&[
        "..H..........H..",
        "..HH........HH..",
        "...bBBBBBBBBb...",
        "...BBBBBBBBBB...",
        "..BBEEBBBBEEBB..",
        "..BBEEBBBBEEBB..",
        "...BBBBBBBBBB...",
        "...BbMTMTMTbB...",
        "....bBBBBBBb....",
        "..bBBBBBBBBBBb..",
        ".CBBb.BBBB.bBBC.",
        ".CBB..BBBB..BBC.",
        ".CC...BBBB...CC.",
        "......bBBb......",
        ".....BB..BB.....",
        "....BB.....BB...",
        "...BB.......BB..",
        "...Bb.......bB..",
        "...BB........BB.",
        "..CC..........CC",
    ]);
    // Arms raised overhead, mouth wide.
    let attack = Sprite::from_art(&[
        ".CC.H......H.CC.",
        ".CBBHH....HHBBC.",
        ".CBBbBBBBBBbBBC.",
        "..BBBBBBBBBBBB..",
        "..BBEEBBBBEEBB..",
        "..bBEEBBBBEEBb..",
        "...BBBBBBBBBB...",
        "...BbMMMMMMbB...",
        "...BbMTMTMTbB...",
        "....bBBBBBBb....",
        "...BBBBBBBBBB...",
        "...BBBBBBBBBB...",
        "....BBBBBBBB....",
        "......bBBb......",
        ".....BB..BB.....",
        "....BB....BB....",
        "....BB....BB....",
        "...bB......Bb...",
        "...BB......BB...",
        "..CC........CC..",
    ]);
    // Flinching: head tilted, eyes shut, blood.
    let pain = Sprite::from_art(&[
        "....H......H....",
        "...HH.....HH....",
        "..bBBBBBBBBb....",
        ".RBBBBBBBBBB....",
        ".RBBMMBBBMMBB...",
        "..rBBBBBBBBBR...",
        "...BBBBBBBBBB...",
        "...BbMMMMMMbB...",
        "....bBBBBBBbR...",
        "..bBBBBBBBBBBb..",
        ".CBBb.BBBB.bBBC.",
        ".CBB..BBBB..BBC.",
        ".CC...BBBB...CC.",
        "......bBBb......",
        ".....BB..BB.....",
        "....BB....BB....",
        "....BB....BB....",
        "...bB......Bb...",
        "...BB......BB...",
        "..CC........CC..",
    ]);
    // Collapsing forward.
    let die_a = Sprite::from_art(&[
        "................",
        "................",
        "................",
        "...H........H...",
        "...HHBBBBBBHH...",
        "..bBBBBBBBBBBb..",
        "..RBMMBBBBMMBR..",
        "..rBBBBBBBBBBr..",
        "...BbMMMMMMbB...",
        "..RbBBBBBBBBbR..",
        ".CBBBBBBBBBBBBC.",
        ".CBb.BBBBBB.bBC.",
        ".CC..BBBBBB..CC.",
        "....bBBBBBBb....",
        "...RBB....BBR...",
        "...BB......BB...",
        "..rB........Br..",
        "..BB........BB..",
        ".RCC........CCR.",
        "................",
    ]);
    let die_b = Sprite::from_art(&[
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "....H......H....",
        "...HHBBBBBHH....",
        "..RbBBBBBBBbR...",
        "..rBMMBBBMMBr...",
        "..RBBBBBBBBBR...",
        ".RbBBBBBBBBBbR..",
        ".CBBBBBBBBBBBC..",
        ".CBbRBBBBBBRbC..",
        "..RR.BBBBB.RR...",
        "...RbBBBBBbR....",
        "..RRBBBBBBRR....",
        ".rRRRbBBbRRRr...",
        "................",
    ]);
    // Flat smear on the floor.
    let corpse = Sprite::from_art(&[
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "....rr..r.......",
        "..rRRrrRRr.r....",
        ".rRbBBBBbRRrr...",
        "rRRBbHbBBbRRRr..",
        ".rrRRbBBbRRrr...",
        "..r.rRRRRr.r....",
        "................",
    ]);
    ImpSprites {
        walk_a,
        walk_b,
        attack,
        pain,
        die_a,
        die_b,
        corpse,
    }
}

/// Gun frame set (drawn bottom-center, view-model style).
pub(super) struct GunSprites {
    pub idle: Sprite,
    pub fire: Sprite,
}

pub(super) fn build_gun_sprites() -> GunSprites {
    // 24x18 pistol held in a gloved hand, slightly right of center.
    let idle = Sprite::from_art(&[
        "........................",
        "..........WGG...........",
        ".........GGGGG..........",
        ".........gGGGGg.........",
        ".........gGGGGg.........",
        ".........gGGGGg.........",
        "........gGGGGGGg........",
        "........gGGGGGGg........",
        ".......gGGGGGGGGg.......",
        ".......sSGGGGGGSs.......",
        "......sSSSGGGGSSSs......",
        ".....sSSSSSGGSSSSSs.....",
        "....sSSSSSSSSSSSSSSs....",
        "....sSSSSSSSSSSSSSs.....",
        "...sSSSSSSSSSSSSSSs.....",
        "...sSSSSSSSSSSSSSs......",
        "..sSSSSSSSSSSSSSSs......",
        "..sSSSSSSSSSSSSSs.......",
    ]);
    let fire = Sprite::from_art(&[
        ".........fFFf...........",
        "........fFFFFf..........",
        ".......fFFFFFFf.........",
        "........fFFFFf..........",
        ".........FGGF...........",
        ".........gGGGGg.........",
        "........gGGGGGGg........",
        "........gGGGGGGg........",
        ".......gGGGGGGGGg.......",
        ".......sSGGGGGGSs.......",
        "......sSSSGGGGSSSs......",
        ".....sSSSSSGGSSSSSs.....",
        "....sSSSSSSSSSSSSSSs....",
        "....sSSSSSSSSSSSSSs.....",
        "...sSSSSSSSSSSSSSSs.....",
        "...sSSSSSSSSSSSSSs......",
        "..sSSSSSSSSSSSSSSs......",
        "..sSSSSSSSSSSSSSs.......",
    ]);
    GunSprites { idle, fire }
}

// -------------------------------------------------------------------------
// 5x7 pixel font (uppercase + the few symbols the game needs)
// -------------------------------------------------------------------------

/// Return the 5x7 glyph rows for a character, MSB-left in the low 5 bits.
/// Unknown characters render as blank.
pub(super) fn glyph5x7(ch: char) -> [u8; 7] {
    match ch.to_ascii_uppercase() {
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'B' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
        'C' => [
            0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110,
        ],
        'D' => [
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'G' => [
            0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111,
        ],
        'H' => [
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'I' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111,
        ],
        'K' => [
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
        'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'M' => [
            0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'P' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        'S' => [
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
        'Y' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        '!' => [
            0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00000, 0b00100,
        ],
        '-' => [
            0b00000, 0b00000, 0b00000, 0b01110, 0b00000, 0b00000, 0b00000,
        ],
        _ => [0; 7],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn textures_have_expected_size() {
        for tex in build_textures() {
            assert_eq!(tex.pixels.len(), TEX_SIZE * TEX_SIZE);
        }
        assert_eq!(build_floor_texture().pixels.len(), TEX_SIZE * TEX_SIZE);
        assert_eq!(build_ceiling_texture().pixels.len(), TEX_SIZE * TEX_SIZE);
    }

    #[test]
    fn xorshift_is_nondegenerate() {
        // Zero seed must not collapse to the all-zero fixed point.
        let mut rng = XorShift64::new(0);
        assert!((0..4).map(|_| rng.next_u32()).any(|v| v != 0));
        // Floats stay in [0, 1).
        let mut rng = XorShift64::new(42);
        for _ in 0..1000 {
            let f = rng.next_f32();
            assert!((0.0..1.0).contains(&f));
        }
    }

    #[test]
    fn every_sprite_palette_char_is_used_in_art() {
        // Keep the palette free of dead entries: every mapped char's color
        // must appear in at least one sprite. Palette colors are distinct,
        // so matching on color is equivalent to matching on char.
        let mut used: std::collections::HashSet<Rgb> = std::collections::HashSet::new();
        let imps = build_imp_sprites();
        let guns = build_gun_sprites();
        for sprite in [
            &imps.walk_a,
            &imps.walk_b,
            &imps.attack,
            &imps.pain,
            &imps.die_a,
            &imps.die_b,
            &imps.corpse,
            &guns.idle,
            &guns.fire,
        ] {
            used.extend(sprite.pixels.iter().flatten());
        }
        for ch in b"BbHEMTCRrGgWSsFf" {
            let color = sprite_color(*ch).expect("palette char must map to a color");
            assert!(
                used.contains(&color),
                "palette char {:?} is mapped but unused in art",
                *ch as char
            );
        }
    }

    #[test]
    fn sprites_have_consistent_rows() {
        // Construction debug-asserts equal row lengths; touch every frame.
        let imps = build_imp_sprites();
        for s in [
            &imps.walk_a,
            &imps.walk_b,
            &imps.attack,
            &imps.pain,
            &imps.die_a,
            &imps.die_b,
            &imps.corpse,
        ] {
            assert_eq!(s.w, 16);
            assert_eq!(s.h, 20);
            // Sampling corners must not panic.
            let _ = s.sample(0.0, 0.0);
            let _ = s.sample(0.999, 0.999);
        }
        let guns = build_gun_sprites();
        assert_eq!(guns.idle.w, 24);
        assert_eq!(guns.fire.w, 24);
    }

    #[test]
    fn font_covers_required_strings() {
        // Every character used by in-game screens must have a glyph.
        for text in [
            "GBOOM",
            "KNEE-DEEP IN THE TOKENS",
            "PRESS ANY KEY",
            "YOU DIED",
            "VICTORY!",
        ] {
            for ch in text.chars().filter(|c| *c != ' ') {
                assert_ne!(
                    glyph5x7(ch),
                    [0; 7],
                    "missing glyph for {ch:?} used in {text:?}"
                );
            }
        }
    }
}
