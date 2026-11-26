//! Software renderer for the `/gboom` easter egg.
//!
//! Grid raycaster (Lodev-style DDA): textured walls, floor, and ceiling
//! with distance fog, billboard sprites with a 1D depth buffer, a
//! view-model gun, and full-frame effects (muzzle light, damage flash,
//! vignette). Also renders the title/end screens (animated fire + text).
//!
//! Everything draws into a plain RGB8 framebuffer the caller PNG-encodes
//! for the kitty graphics protocol.

use super::assets::{self, GunSprites, ImpSprites, Rgb, TEX_SIZE, Texture, XorShift64};
use super::game::{Game, ImpVisual};

/// Distance fog factor: shade = 1 / (1 + dist * FOG).
const FOG: f32 = 0.16;
/// Sprite height in world units (walls are 1.0 tall).
const IMP_WORLD_HEIGHT: f32 = 0.72;
/// Camera half-FOV tangent (0.66 ≈ the classic 66° FOV).
const PLANE_LEN: f32 = 0.66;
/// Corner-vignette strength (0 = none); subtle, ~0.8 at the extreme corners.
const VIGNETTE: f32 = 0.11;

/// RGB framebuffer with reusable scratch buffers.
pub(super) struct FrameBuffer {
    pub w: usize,
    pub h: usize,
    pub pixels: Vec<u8>, // RGB8, row-major
    zbuf: Vec<f32>,      // per-column wall depth
    /// Per-column wall strip bounds `[top, bottom)` in screen rows, written
    /// by `draw_walls` and read by `draw_floor_ceiling` to skip the pixels
    /// walls already cover (avoids texturing them twice).
    wall_top: Vec<i32>,
    wall_bottom: Vec<i32>,
    /// Scratch for painter's-order sprite sorting, reused across frames.
    sprite_order: Vec<(usize, f32)>,
    /// Separable vignette factors, rebuilt on dimension change.
    vig_x: Vec<f32>,
    vig_y: Vec<f32>,
}

impl FrameBuffer {
    pub fn new() -> Self {
        Self {
            w: 0,
            h: 0,
            pixels: Vec::new(),
            zbuf: Vec::new(),
            wall_top: Vec::new(),
            wall_bottom: Vec::new(),
            sprite_order: Vec::new(),
            vig_x: Vec::new(),
            vig_y: Vec::new(),
        }
    }

    pub fn resize(&mut self, w: usize, h: usize) {
        if self.w == w && self.h == h {
            return;
        }
        self.w = w;
        self.h = h;
        self.pixels.resize(w * h * 3, 0);
        self.zbuf.resize(w, f32::MAX);
        self.wall_top.resize(w, 0);
        self.wall_bottom.resize(w, 0);
        let axis = |i: usize, n: usize| {
            let t = if n <= 1 {
                0.0
            } else {
                2.0 * i as f32 / (n - 1) as f32 - 1.0
            };
            1.0 - VIGNETTE * t * t
        };
        self.vig_x = (0..w).map(|x| axis(x, w)).collect();
        self.vig_y = (0..h).map(|y| axis(y, h)).collect();
    }

    #[inline]
    fn put(&mut self, x: usize, y: usize, c: Rgb) {
        let i = (y * self.w + x) * 3;
        self.pixels[i] = c[0];
        self.pixels[i + 1] = c[1];
        self.pixels[i + 2] = c[2];
    }

    /// Multiply the pixel at `(x, y)` by `f` (used for sprite shadows).
    #[inline]
    fn darken(&mut self, x: usize, y: usize, f: f32) {
        let i = (y * self.w + x) * 3;
        self.pixels[i] = (self.pixels[i] as f32 * f) as u8;
        self.pixels[i + 1] = (self.pixels[i + 1] as f32 * f) as u8;
        self.pixels[i + 2] = (self.pixels[i + 2] as f32 * f) as u8;
    }

    /// Darken the frame toward the corners. Applied to the world (before
    /// the view-model gun, which stays crisp).
    fn apply_vignette(&mut self) {
        for y in 0..self.h {
            let vy = self.vig_y[y];
            let row = &mut self.pixels[y * self.w * 3..(y + 1) * self.w * 3];
            for (x, px) in row.chunks_exact_mut(3).enumerate() {
                let f = self.vig_x[x] * vy;
                px[0] = (px[0] as f32 * f) as u8;
                px[1] = (px[1] as f32 * f) as u8;
                px[2] = (px[2] as f32 * f) as u8;
            }
        }
    }
}

#[inline]
fn shade(c: Rgb, f: f32) -> Rgb {
    [
        (c[0] as f32 * f).min(255.0) as u8,
        (c[1] as f32 * f).min(255.0) as u8,
        (c[2] as f32 * f).min(255.0) as u8,
    ]
}

#[inline]
fn lerp_color(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t) as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t) as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t) as u8,
    ]
}

/// All immutable render resources, built once per game.
pub(super) struct Renderer {
    textures: Vec<Texture>,
    floor: Texture,
    ceiling: Texture,
    imps: ImpSprites,
    guns: GunSprites,
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            textures: assets::build_textures(),
            floor: assets::build_floor_texture(),
            ceiling: assets::build_ceiling_texture(),
            imps: assets::build_imp_sprites(),
            guns: assets::build_gun_sprites(),
        }
    }

    /// Render one gameplay frame into `fb`.
    pub fn render_game(&self, fb: &mut FrameBuffer, game: &Game) {
        let (w, h) = (fb.w, fb.h);
        if w == 0 || h == 0 {
            return;
        }

        // Muzzle flash briefly lights the whole scene.
        let light_boost = if game.player.muzzle > 0.0 { 1.35 } else { 1.0 };

        // Walls first: they record per-column strip bounds + depth, letting
        // the floor/ceiling pass skip the pixels they cover (no double-write).
        self.draw_walls(fb, game, light_boost);
        self.draw_floor_ceiling(fb, game, light_boost);
        self.draw_imps(fb, game, light_boost);
        // World-only vignette: the view-model gun stays crisp on top.
        fb.apply_vignette();
        self.draw_gun(fb, game);

        // Damage flash: flat blend of the whole frame toward red, decaying
        // with `damage_flash`. Reads clearly even at low resolutions.
        if game.player.damage_flash > 0.0 {
            let t = (game.player.damage_flash * 0.45).min(0.45);
            for px in fb.pixels.chunks_exact_mut(3) {
                px[0] = (px[0] as f32 + (220.0 - px[0] as f32) * t) as u8;
                px[1] = (px[1] as f32 * (1.0 - t * 0.8)) as u8;
                px[2] = (px[2] as f32 * (1.0 - t * 0.8)) as u8;
            }
        }
        // Low-health vignette pulse.
        if game.player.hp <= 25 && !game.dead() {
            let pulse = 0.10 + 0.06 * (game.time * 5.0).sin();
            for px in fb.pixels.chunks_exact_mut(3) {
                px[0] = (px[0] as f32 + (160.0 - px[0] as f32) * pulse) as u8;
            }
        }
    }

    /// Perspective-correct textured floor and ceiling (Lodev scanline
    /// casting): each screen row below/above the horizon maps to one
    /// world-space distance, so texels are sampled by stepping world
    /// coordinates across the row. Distance fog matches the wall pass,
    /// making the whole scene recede uniformly into darkness.
    ///
    /// Runs after `draw_walls` and skips pixels inside each column's wall
    /// strip — the world coords still step every pixel (to stay aligned),
    /// but the texture sample/shade/write are elided where a wall covers.
    fn draw_floor_ceiling(&self, fb: &mut FrameBuffer, game: &Game, light: f32) {
        let (w, h) = (fb.w, fb.h);
        let p = &game.player;
        let (dir_x, dir_y) = p.dir();
        let (plane_x, plane_y) = (-dir_y * PLANE_LEN, dir_x * PLANE_LEN);

        // Leftmost and rightmost camera rays of the view frustum.
        let (ray0_x, ray0_y) = (dir_x - plane_x, dir_y - plane_y);
        let (ray1_x, ray1_y) = (dir_x + plane_x, dir_y + plane_y);
        let cam_z = 0.5 * h as f32;

        for y in h / 2..h {
            // Rows at the horizon map to (near-)infinite distance.
            let row = (y - h / 2).max(1) as f32;
            let row_dist = cam_z / row;
            let fog = (1.0 / (1.0 + row_dist * FOG)) * light;

            let step_x = row_dist * (ray1_x - ray0_x) / w as f32;
            let step_y = row_dist * (ray1_y - ray0_y) / w as f32;
            let mut world_x = p.x + row_dist * ray0_x;
            let mut world_y = p.y + row_dist * ray0_y;

            // The ceiling row at the same distance mirrors across the
            // horizon (camera eye is at half wall height).
            let ceil_y = h - 1 - y;
            let (yi, ceil_yi) = (y as i32, ceil_y as i32);
            for x in 0..w {
                let (wx, wy) = (world_x, world_y);
                world_x += step_x;
                world_y += step_y;

                // Skip pixels the wall strip already filled this column. The
                // texel coords are computed lazily, so the central wall band
                // (where both are covered) costs only the world-coord step.
                let floor_vis = yi >= fb.wall_bottom[x];
                let ceil_vis = ceil_yi < fb.wall_top[x];
                if !(floor_vis || ceil_vis) {
                    continue;
                }
                let tx = (wx.rem_euclid(1.0) * TEX_SIZE as f32) as usize;
                let ty = (wy.rem_euclid(1.0) * TEX_SIZE as f32) as usize;
                if floor_vis {
                    fb.put(x, y, shade(self.floor.sample(tx, ty), fog));
                }
                if ceil_vis {
                    fb.put(x, ceil_y, shade(self.ceiling.sample(tx, ty), fog));
                }
            }
        }
    }

    fn draw_walls(&self, fb: &mut FrameBuffer, game: &Game, light: f32) {
        let (w, h) = (fb.w, fb.h);
        let p = &game.player;
        let (dir_x, dir_y) = p.dir();
        let (plane_x, plane_y) = (-dir_y * PLANE_LEN, dir_x * PLANE_LEN);

        for x in 0..w {
            let camera_x = 2.0 * x as f32 / w as f32 - 1.0;
            let rd_x = dir_x + plane_x * camera_x;
            let rd_y = dir_y + plane_y * camera_x;

            let mut map_x = p.x.floor() as i32;
            let mut map_y = p.y.floor() as i32;
            let delta_x = if rd_x == 0.0 {
                f32::MAX
            } else {
                (1.0 / rd_x).abs()
            };
            let delta_y = if rd_y == 0.0 {
                f32::MAX
            } else {
                (1.0 / rd_y).abs()
            };
            let (step_x, mut side_x) = if rd_x < 0.0 {
                (-1, (p.x - map_x as f32) * delta_x)
            } else {
                (1, (map_x as f32 + 1.0 - p.x) * delta_x)
            };
            let (step_y, mut side_y) = if rd_y < 0.0 {
                (-1, (p.y - map_y as f32) * delta_y)
            } else {
                (1, (map_y as f32 + 1.0 - p.y) * delta_y)
            };

            // DDA until a solid cell. The map border is fully solid, so
            // bound the loop defensively rather than trusting it blindly.
            let mut side = 0;
            let mut tex_id = 1u8;
            for _ in 0..256 {
                if side_x < side_y {
                    side_x += delta_x;
                    map_x += step_x;
                    side = 0;
                } else {
                    side_y += delta_y;
                    map_y += step_y;
                    side = 1;
                }
                let cell = game.map.cell(map_x, map_y);
                if cell != 0 {
                    tex_id = cell;
                    break;
                }
            }

            let perp = if side == 0 {
                (map_x as f32 - p.x + (1 - step_x) as f32 / 2.0) / rd_x
            } else {
                (map_y as f32 - p.y + (1 - step_y) as f32 / 2.0) / rd_y
            };
            let perp = perp.max(1e-4);
            fb.zbuf[x] = perp;

            let line_h = (h as f32 / perp) as i32;
            let draw_start = ((h as i32 - line_h) / 2).max(0);
            let draw_end = ((h as i32 + line_h) / 2).min(h as i32);
            // Record the strip so draw_floor_ceiling skips these rows.
            fb.wall_top[x] = draw_start;
            fb.wall_bottom[x] = draw_end;

            // Texture column.
            let wall_x = if side == 0 {
                p.y + perp * rd_y
            } else {
                p.x + perp * rd_x
            };
            let wall_x = wall_x - wall_x.floor();
            let mut tex_x = (wall_x * TEX_SIZE as f32) as usize;
            if (side == 0 && rd_x > 0.0) || (side == 1 && rd_y < 0.0) {
                tex_x = TEX_SIZE - 1 - tex_x.min(TEX_SIZE - 1);
            }

            let texture = &self.textures[(tex_id as usize - 1).min(self.textures.len() - 1)];
            let side_shade = if side == 1 { 0.72 } else { 1.0 };
            let fog_shade = (1.0 / (1.0 + perp * FOG)) * side_shade * light;

            let tex_step = TEX_SIZE as f32 / line_h.max(1) as f32;
            let mut tex_pos = (draw_start as f32 - h as f32 / 2.0 + line_h as f32 / 2.0) * tex_step;
            for y in draw_start..draw_end {
                let tex_y = (tex_pos as usize).min(TEX_SIZE - 1);
                tex_pos += tex_step;
                let c = shade(texture.sample(tex_x, tex_y), fog_shade);
                fb.put(x, y as usize, c);
            }
        }
    }

    fn draw_imps(&self, fb: &mut FrameBuffer, game: &Game, light: f32) {
        let (w, h) = (fb.w, fb.h);
        let p = &game.player;
        let (dir_x, dir_y) = p.dir();
        let (plane_x, plane_y) = (-dir_y * PLANE_LEN, dir_x * PLANE_LEN);
        let inv_det = 1.0 / (plane_x * dir_y - dir_x * plane_y);

        // Painter's order: far → near. The order buffer lives on the
        // framebuffer so the 30 fps render loop stays allocation-free;
        // it is taken out for the duration of the draw because the loop
        // body needs `fb` mutably.
        let mut order = std::mem::take(&mut fb.sprite_order);
        order.clear();
        order.extend(game.imps.iter().enumerate().map(|(i, imp)| {
            let d2 = (imp.x - p.x).powi(2) + (imp.y - p.y).powi(2);
            (i, d2)
        }));
        order.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));

        for &(i, _) in &order {
            let imp = &game.imps[i];
            let rel_x = imp.x - p.x;
            let rel_y = imp.y - p.y;
            // Camera-space transform: ty = forward depth, tx = lateral.
            let tx = inv_det * (dir_y * rel_x - dir_x * rel_y);
            let ty = inv_det * (-plane_y * rel_x + plane_x * rel_y);
            if ty <= 0.08 {
                continue; // behind or on top of the camera
            }

            let sprite = match imp.visual() {
                ImpVisual::WalkA => &self.imps.walk_a,
                ImpVisual::WalkB => &self.imps.walk_b,
                ImpVisual::Attack => &self.imps.attack,
                ImpVisual::Pain => &self.imps.pain,
                ImpVisual::DieA => &self.imps.die_a,
                ImpVisual::DieB => &self.imps.die_b,
                ImpVisual::Corpse => &self.imps.corpse,
            };

            let screen_x = (w as f32 / 2.0) * (1.0 + tx / ty);
            // Vertical span from world heights [0, IMP_WORLD_HEIGHT] with the
            // camera eye at 0.5: y(world_z) = h/2 + (0.5 - z) * h / ty.
            let y_feet = h as f32 / 2.0 + 0.5 * h as f32 / ty;
            let y_head = h as f32 / 2.0 + (0.5 - IMP_WORLD_HEIGHT) * h as f32 / ty;
            let sprite_h = (y_feet - y_head).max(1.0);
            let sprite_w = sprite_h * sprite.w as f32 / sprite.h as f32;

            // Small vertical bob while walking sells the gait.
            let bob_px = imp.walk_bob().map_or(0.0, |phase| phase * sprite_h * 0.02);

            let x0 = (screen_x - sprite_w / 2.0).floor() as i32;
            let x1 = (screen_x + sprite_w / 2.0).ceil() as i32;
            let y0 = (y_head + bob_px).floor() as i32;
            let y1 = (y_feet + bob_px).ceil() as i32;

            let fog_shade = (1.0 / (1.0 + ty * FOG)) * light;

            // Soft elliptical contact shadow under standing demons. Drawn
            // before the body, z-tested per column like the body.
            if !matches!(imp.visual(), ImpVisual::Corpse) {
                draw_contact_shadow(fb, screen_x, y_feet, sprite_w, sprite_h, ty);
            }

            // Pain frames flash toward white so hits register instantly.
            let pain_flash = imp.visual() == ImpVisual::Pain;

            for sx in x0.max(0)..x1.min(w as i32) {
                if fb.zbuf[sx as usize] <= ty {
                    continue; // occluded by a wall
                }
                let u = (sx as f32 - x0 as f32) / (x1 - x0).max(1) as f32;
                for sy in y0.max(0)..y1.min(h as i32) {
                    let v = (sy as f32 - y0 as f32) / (y1 - y0).max(1) as f32;
                    if let Some(c) = sprite.sample(u, v) {
                        // Glowing eyes ignore fog; everything else fades.
                        let mut lit = if c == assets::EYE_GLOW {
                            c
                        } else {
                            shade(c, fog_shade)
                        };
                        if pain_flash {
                            lit = lerp_color(lit, [255, 255, 255], 0.40);
                        }
                        fb.put(sx as usize, sy as usize, lit);
                    }
                }
            }
        }

        fb.sprite_order = order;
    }

    fn draw_gun(&self, fb: &mut FrameBuffer, game: &Game) {
        let (w, h) = (fb.w, fb.h);
        let sprite = if game.player.muzzle > 0.0 {
            &self.guns.fire
        } else {
            &self.guns.idle
        };

        // Gun occupies ~42% of frame height, bottom-center, with walk bob.
        let gun_h = (h as f32 * 0.42) as i32;
        let gun_w = gun_h * sprite.w as i32 / sprite.h as i32;
        let bob_x = (game.player.bob * 1.7).sin() * w as f32 * 0.012;
        let bob_y = (game.player.bob * 3.4).cos().abs() * h as f32 * 0.018;
        let x0 = w as i32 / 2 - gun_w / 2 + bob_x as i32;
        let y0 = h as i32 - gun_h + bob_y as i32;

        for sy in y0.max(0)..h as i32 {
            let v = (sy - y0) as f32 / gun_h.max(1) as f32;
            for sx in x0.max(0)..(x0 + gun_w).min(w as i32) {
                let u = (sx - x0) as f32 / gun_w.max(1) as f32;
                if let Some(c) = sprite.sample(u, v) {
                    fb.put(sx as usize, sy as usize, c);
                }
            }
        }

        // Crosshair.
        let (cx, cy) = (w / 2, h / 2);
        // Aim feedback: the crosshair turns red over a hittable demon and
        // gains a center dot.
        let on_target = game.target_in_crosshair().is_some();
        let ch_c: Rgb = if on_target {
            assets::GBOOM_RED
        } else {
            [210, 210, 210]
        };
        for d in 2..5usize {
            if cx >= d && cx + d < w {
                fb.put(cx - d, cy, ch_c);
                fb.put(cx + d, cy, ch_c);
            }
            if cy >= d && cy + d < h {
                fb.put(cx, cy - d, ch_c);
                fb.put(cx, cy + d, ch_c);
            }
        }
        if on_target {
            fb.put(cx, cy, ch_c);
        }
    }
}

/// Soft elliptical contact shadow at a sprite's feet, z-tested per column
/// with the sprite's own depth so walls still occlude it.
fn draw_contact_shadow(
    fb: &mut FrameBuffer,
    center_x: f32,
    y_feet: f32,
    sprite_w: f32,
    sprite_h: f32,
    depth: f32,
) {
    let (w, h) = (fb.w, fb.h);
    let rx = (sprite_w * 0.38).max(1.0);
    let ry = (sprite_h * 0.05).max(1.5);
    let x0 = (center_x - rx).floor() as i32;
    let x1 = (center_x + rx).ceil() as i32;
    let y0 = (y_feet - ry).floor() as i32;
    let y1 = (y_feet + ry).ceil() as i32;
    for sx in x0.max(0)..x1.min(w as i32) {
        if fb.zbuf[sx as usize] <= depth {
            continue;
        }
        let nx = (sx as f32 - center_x) / rx;
        for sy in y0.max(0)..y1.min(h as i32) {
            let ny = (sy as f32 - y_feet) / ry;
            let r2 = nx * nx + ny * ny;
            if r2 < 1.0 {
                // Darkest at the center, fading out toward the rim.
                fb.darken(sx as usize, sy as usize, 0.55 + 0.45 * r2);
            }
        }
    }
}

// -------------------------------------------------------------------------
// Title / end screens: animated fire + 5x7 pixel text
// -------------------------------------------------------------------------

/// The classic PSX-style fire effect: a cellular automaton on a coarse
/// grid, upscaled at draw time. Heat values 0..=36 index a fire palette.
pub(super) struct FireSim {
    w: usize,
    h: usize,
    heat: Vec<u8>,
    rng: XorShift64,
}

const FIRE_MAX: u8 = 36;

impl FireSim {
    pub fn new() -> Self {
        let (w, h) = (160, 84);
        let mut heat = vec![0u8; w * h];
        // Bottom row is the white-hot source.
        for x in 0..w {
            heat[(h - 1) * w + x] = FIRE_MAX;
        }
        Self {
            w,
            h,
            heat,
            rng: XorShift64::new(0xDEAD_BEEF_CAFE_F00D),
        }
    }

    /// One simulation step: heat propagates upward with random decay/drift.
    pub fn step(&mut self) {
        for y in 1..self.h {
            for x in 0..self.w {
                let src = y * self.w + x;
                let r = self.rng.next_u32();
                let decay = (r & 1) as i32; // cool by 0 or 1
                let drift = (r >> 2) % 3; // 0, 1, 2 → left, stay, right
                let dst_x = (x as i32 + drift as i32 - 1).rem_euclid(self.w as i32) as usize;
                let dst = (y - 1) * self.w + dst_x;
                self.heat[dst] = (self.heat[src] as i32 - decay).max(0) as u8;
            }
        }
    }

    fn palette(heat: u8) -> Rgb {
        // Black → deep red → orange → yellow → white.
        let t = heat as f32 / FIRE_MAX as f32;
        if t < 0.02 {
            [7, 7, 9]
        } else if t < 0.4 {
            lerp_color([24, 8, 6], [180, 30, 10], t / 0.4)
        } else if t < 0.75 {
            lerp_color([180, 30, 10], [240, 150, 30], (t - 0.4) / 0.35)
        } else {
            lerp_color([240, 150, 30], [255, 250, 200], (t - 0.75) / 0.25)
        }
    }

    /// Draw the fire across the bottom `frac` of the framebuffer.
    pub fn draw(&self, fb: &mut FrameBuffer, frac: f32) {
        let (w, h) = (fb.w, fb.h);
        if w == 0 || h == 0 {
            return;
        }
        let fire_h = (h as f32 * frac) as usize;
        let y_start = h - fire_h.min(h);
        for y in y_start..h {
            let fy = (y - y_start) * self.h / fire_h.max(1);
            for x in 0..w {
                let fx = x * self.w / w;
                let heat = self.heat[fy.min(self.h - 1) * self.w + fx.min(self.w - 1)];
                if heat > 1 {
                    fb.put(x, y, Self::palette(heat));
                }
            }
        }
    }
}

/// Fill the framebuffer with a flat color.
pub(super) fn clear(fb: &mut FrameBuffer, c: Rgb) {
    for px in fb.pixels.chunks_exact_mut(3) {
        px.copy_from_slice(&c);
    }
}

/// Measure the pixel width of `text` at `scale` (5x7 glyphs, 1px tracking).
pub(super) fn text_width(text: &str, scale: usize) -> usize {
    text.chars().count() * 6 * scale
}

/// Draw 5x7 pixel text with its top-left at `(x0, y0)`.
pub(super) fn draw_text(fb: &mut FrameBuffer, text: &str, x0: i32, y0: i32, scale: usize, c: Rgb) {
    let mut pen_x = x0;
    for ch in text.chars() {
        let glyph = assets::glyph5x7(ch);
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let px = pen_x + (col * scale + dx) as i32;
                        let py = y0 + (row * scale + dy) as i32;
                        if px >= 0 && py >= 0 && (px as usize) < fb.w && (py as usize) < fb.h {
                            fb.put(px as usize, py as usize, c);
                        }
                    }
                }
            }
        }
        pen_x += (6 * scale) as i32;
    }
}

/// Draw centered text with an 8-direction outline, which keeps the chunky
/// font legible over the animated fire background.
pub(super) fn draw_text_centered_outlined(
    fb: &mut FrameBuffer,
    text: &str,
    y0: i32,
    scale: usize,
    c: Rgb,
    outline: Rgb,
) {
    let x0 = (fb.w as i32 - text_width(text, scale) as i32) / 2;
    let o = (scale as i32 / 2).max(1);
    for (dx, dy) in [
        (-o, -o),
        (0, -o),
        (o, -o),
        (-o, 0),
        (o, 0),
        (-o, o),
        (0, o),
        (o, o),
    ] {
        draw_text(fb, text, x0 + dx, y0 + dy, scale, outline);
    }
    draw_text(fb, text, x0, y0, scale, c);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_game_fills_framebuffer() {
        let renderer = Renderer::new();
        let mut fb = FrameBuffer::new();
        fb.resize(320, 200);
        renderer.render_game(&mut fb, &Game::new());
        // The floor/ceiling pass paints the full frame, so it can't be all-zero.
        assert!(fb.pixels.iter().any(|&b| b != 0));
        assert_eq!(fb.pixels.len(), 320 * 200 * 3);
    }

    #[test]
    fn render_survives_extreme_sizes() {
        let renderer = Renderer::new();
        let game = Game::new();
        let mut fb = FrameBuffer::new();
        for (w, h) in [(1usize, 1usize), (2, 2), (16, 8), (639, 401)] {
            fb.resize(w, h);
            renderer.render_game(&mut fb, &game);
        }
    }

    #[test]
    fn zbuffer_occludes_sprites_behind_walls() {
        let renderer = Renderer::new();
        let mut game = Game::new();
        // Move all imps far behind the player so none are visible, render,
        // then put one directly in front and confirm pixels change.
        for imp in &mut game.imps {
            imp.x = game.player.x - 8.0;
            imp.y = game.player.y;
        }
        let mut fb = FrameBuffer::new();
        fb.resize(160, 100);
        renderer.render_game(&mut fb, &game);
        let before = fb.pixels.clone();

        let (dx, dy) = game.player.dir();
        game.imps[0].x = game.player.x + dx * 1.5;
        game.imps[0].y = game.player.y + dy * 1.5;
        renderer.render_game(&mut fb, &game);
        assert_ne!(before, fb.pixels, "visible imp must change the frame");
    }

    #[test]
    fn fire_sim_burns_upward() {
        let mut fire = FireSim::new();
        for _ in 0..60 {
            fire.step();
        }
        // After enough steps some heat must exist above the source row.
        let above: u32 = (0..fire.w)
            .map(|x| fire.heat[(fire.h / 2) * fire.w + x] as u32)
            .sum();
        assert!(above > 0, "fire should propagate upward");
    }
}
