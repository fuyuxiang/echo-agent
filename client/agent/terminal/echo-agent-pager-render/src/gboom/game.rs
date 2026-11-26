//! World simulation for the `/gboom` easter egg: map, momentum-based player
//! movement, imp AI, and hitscan combat.
//!
//! `step(dt)` advances by wall-clock time, so the game plays identically at
//! any frame rate.

/// One hand-authored level. Digits are walls and pick the texture:
/// `1` brick, `2` stone, `3` tech, `4` hellstone. `.` is floor, `P` the
/// player start, `I` an imp spawn. Spawn reachability is enforced by test.
const MAP_ART: &[&str] = &[
    "1111111111111111111111",
    "1P...1....2......2...1",
    "1....1.22.2.3333.2.I.1",
    "1....1.2..2.3..3.2...1",
    "1.11.1.2.I2.3.I3.222.1",
    "1.1..1.2..2.33.3...2.1",
    "1.1..1.22.2....3.2.2.1",
    "1.1......2..33.3.2...1",
    "1.111111.2.I3..32222.1",
    "1......1.2..3333.....1",
    "144444.1.2........11.1",
    "1....4.1.22222222..1.1",
    "1.I..4.1........2.I1.1",
    "1....4.11111111.2..1.1",
    "1.4444.......41.2222.1",
    "1.4..444444..41......1",
    "1.4.I......I.41.111111",
    "1.4..444444..4....I..1",
    "1.444........4.11....1",
    "1...44444444.4.1..1111",
    "1............4.1.....1",
    "1111111111111111111111",
];

/// Player collision radius, in tiles. A touch under a quarter-tile so the
/// 1-tile-wide corridors have comfortable clearance.
const PLAYER_RADIUS: f32 = 0.20;
/// Imp collision radius, in tiles.
const IMP_RADIUS: f32 = 0.30;

/// Movement tuning — continuous "held" model.
///
/// With no key-release events, each press/repeat refreshes a per-control
/// hold timer ([`HOLD_WINDOW`]); while it's positive, velocity eases toward
/// a steady target. A constant target while held means speed doesn't
/// sawtooth with the OS key-repeat cadence, yet releasing glides to a stop.
const MOVE_SPEED: f32 = 3.3; // tiles/s while a move key is held
const TURN_SPEED: f32 = 2.2; // rad/s (~125°/s) while a turn key is held
/// Velocity-smoothing time constants (seconds). Small = snappy response
/// with just enough ramp to read as momentum rather than teleporting.
const MOVE_ACCEL_TAU: f32 = 0.08;
const TURN_ACCEL_TAU: f32 = 0.07;
/// How long after each press/repeat a control stays "held". Must exceed
/// the slowest expected key-repeat interval (≈30–60 ms) so motion never
/// stutters between repeats, while staying short enough that releasing
/// stops promptly.
const HOLD_WINDOW: f32 = 0.16;

const PLAYER_MAX_HP: i32 = 100;
const FIRE_COOLDOWN: f32 = 0.32;
const MUZZLE_TIME: f32 = 0.09;
/// Hitscan half-width: an imp is hit when its center is within this
/// perpendicular distance of the aim ray.
const HIT_WIDTH: f32 = 0.33;
const PISTOL_DAMAGE: i32 = 11;

const IMP_HP: i32 = 30;
const IMP_SPEED: f32 = 1.55;
const IMP_SIGHT_RANGE: f32 = 9.0;
const IMP_MELEE_RANGE: f32 = 0.95;
const IMP_WINDUP: f32 = 0.38;
const IMP_ATTACK_COOLDOWN: f32 = 0.95;
const IMP_PAIN_TIME: f32 = 0.28;
const IMP_DEATH_TIME: f32 = 0.55;
const IMP_BITE_DAMAGE: i32 = 7;

/// Grid map with texture-id cells.
pub(super) struct Map {
    pub w: usize,
    pub h: usize,
    cells: Vec<u8>, // 0 = floor, 1..=4 = wall texture id
}

impl Map {
    /// Wall texture id at a cell, or 0 for floor. Out of bounds is solid.
    #[inline]
    pub fn cell(&self, x: i32, y: i32) -> u8 {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return 1;
        }
        self.cells[y as usize * self.w + x as usize]
    }

    #[inline]
    pub fn solid(&self, x: i32, y: i32) -> bool {
        self.cell(x, y) != 0
    }

    /// Whether a circle of `radius` at `(x, y)` overlaps any solid cell.
    fn blocked(&self, x: f32, y: f32, radius: f32) -> bool {
        let min_x = (x - radius).floor() as i32;
        let max_x = (x + radius).floor() as i32;
        let min_y = (y - radius).floor() as i32;
        let max_y = (y + radius).floor() as i32;
        for cy in min_y..=max_y {
            for cx in min_x..=max_x {
                if self.solid(cx, cy) {
                    return true;
                }
            }
        }
        false
    }

    /// Line-of-sight check between two points (wall occlusion only).
    /// Standard DDA over grid cells.
    pub fn los(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> bool {
        let dx = x1 - x0;
        let dy = y1 - y0;
        let dist = (dx * dx + dy * dy).sqrt();
        if dist < 1e-4 {
            return true;
        }
        let (rdx, rdy) = (dx / dist, dy / dist);
        let mut map_x = x0.floor() as i32;
        let mut map_y = y0.floor() as i32;
        let delta_x = if rdx == 0.0 {
            f32::MAX
        } else {
            (1.0 / rdx).abs()
        };
        let delta_y = if rdy == 0.0 {
            f32::MAX
        } else {
            (1.0 / rdy).abs()
        };
        let (step_x, mut side_x) = if rdx < 0.0 {
            (-1, (x0 - map_x as f32) * delta_x)
        } else {
            (1, (map_x as f32 + 1.0 - x0) * delta_x)
        };
        let (step_y, mut side_y) = if rdy < 0.0 {
            (-1, (y0 - map_y as f32) * delta_y)
        } else {
            (1, (map_y as f32 + 1.0 - y0) * delta_y)
        };
        loop {
            let travelled = if side_x < side_y {
                map_x += step_x;
                let t = side_x;
                side_x += delta_x;
                t
            } else {
                map_y += step_y;
                let t = side_y;
                side_y += delta_y;
                t
            };
            if travelled >= dist {
                return true;
            }
            if self.solid(map_x, map_y) {
                return false;
            }
        }
    }
}

/// Player state.
pub(super) struct Player {
    pub x: f32,
    pub y: f32,
    pub angle: f32,
    /// Forward velocity (negative = backpedal), tiles/s.
    pub vel_forward: f32,
    /// Strafe velocity (positive = right), tiles/s.
    pub vel_strafe: f32,
    /// Angular velocity, rad/s (positive = turn right).
    pub vel_rot: f32,
    pub hp: i32,
    /// Seconds until the pistol can fire again.
    fire_cooldown: f32,
    /// Remaining muzzle-flash display time.
    pub muzzle: f32,
    /// Damage flash intensity, decays to 0.
    pub damage_flash: f32,
    /// Accumulated distance for view/gun bobbing.
    pub bob: f32,
}

impl Player {
    #[inline]
    pub fn dir(&self) -> (f32, f32) {
        (self.angle.cos(), self.angle.sin())
    }
}

/// Movement controls, fed by key press/repeat events. The discriminants
/// double as indices into [`Game::hold`], so keep them field-less.
#[derive(Clone, Copy, Debug)]
pub(super) enum Control {
    Forward,
    Back,
    TurnLeft,
    TurnRight,
    StrafeLeft,
    StrafeRight,
}

impl Control {
    /// Number of controls — the size of the hold-timer array.
    const COUNT: usize = 6;
}

/// What an imp is doing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum ImpState {
    Idle,
    Chasing,
    /// Wind-up before a melee hit; `t` counts down.
    Attacking {
        t: f32,
    },
    Pain {
        t: f32,
    },
    Dying {
        t: f32,
    },
    Dead,
}

/// Which sprite frame to draw for an imp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ImpVisual {
    WalkA,
    WalkB,
    Attack,
    Pain,
    DieA,
    DieB,
    Corpse,
}

pub(super) struct Imp {
    pub x: f32,
    pub y: f32,
    pub hp: i32,
    pub state: ImpState,
    /// Walk-cycle clock.
    anim: f32,
    /// Seconds until the next melee attempt is allowed.
    attack_cooldown: f32,
}

impl Imp {
    pub fn alive(&self) -> bool {
        !matches!(self.state, ImpState::Dying { .. } | ImpState::Dead)
    }

    /// Walk-cycle bob phase in `[-1, 1]`, or `None` when not walking.
    /// Drives a small vertical offset in the renderer.
    pub fn walk_bob(&self) -> Option<f32> {
        matches!(self.state, ImpState::Chasing).then(|| (self.anim * 9.0).sin())
    }

    pub fn visual(&self) -> ImpVisual {
        match self.state {
            ImpState::Idle => ImpVisual::WalkA,
            ImpState::Chasing => {
                if ((self.anim * 3.0) as u32).is_multiple_of(2) {
                    ImpVisual::WalkA
                } else {
                    ImpVisual::WalkB
                }
            }
            ImpState::Attacking { .. } => ImpVisual::Attack,
            ImpState::Pain { .. } => ImpVisual::Pain,
            ImpState::Dying { t } => {
                if t > IMP_DEATH_TIME * 0.5 {
                    ImpVisual::DieA
                } else {
                    ImpVisual::DieB
                }
            }
            ImpState::Dead => ImpVisual::Corpse,
        }
    }
}

/// The whole game world.
pub(super) struct Game {
    pub map: Map,
    pub player: Player,
    pub imps: Vec<Imp>,
    pub kills: u32,
    pub time: f32,
    /// Per-control "held" countdown in seconds, indexed by `Control as
    /// usize`. Positive means held. In timer mode each press/repeat
    /// refreshes it to [`HOLD_WINDOW`] and `step` decrements it; in
    /// release-aware mode a press latches it (and only [`Game::release`]
    /// clears it), so several keys can be held at once.
    hold: [f32; Control::COUNT],
    /// Whether the terminal delivers key-release events (Kitty keyboard
    /// protocol). When true, controls are latched on press and cleared on
    /// release — enabling true simultaneous move + turn. When false, the
    /// timer bridges the gaps between OS key-repeats for a single key.
    release_aware: bool,
    /// Set by [`Game::queue_fire`], consumed by the next `step`.
    fire_queued: bool,
    rng: super::assets::XorShift64,
}

impl Game {
    pub fn new() -> Self {
        let h = MAP_ART.len();
        let w = MAP_ART[0].len();
        let mut cells = vec![0u8; w * h];
        let mut player_start = (1.5f32, 1.5f32);
        let mut imps = Vec::new();
        for (y, row) in MAP_ART.iter().enumerate() {
            debug_assert_eq!(row.len(), w, "map rows must be equal length");
            for (x, ch) in row.bytes().enumerate() {
                let center = (x as f32 + 0.5, y as f32 + 0.5);
                match ch {
                    b'1'..=b'4' => cells[y * w + x] = ch - b'0',
                    b'P' => player_start = center,
                    b'I' => imps.push(Imp {
                        x: center.0,
                        y: center.1,
                        hp: IMP_HP,
                        state: ImpState::Idle,
                        anim: (x * 7 + y * 13) as f32 * 0.1, // desync walk cycles
                        attack_cooldown: 0.0,
                    }),
                    _ => {}
                }
            }
        }
        Self {
            map: Map { w, h, cells },
            player: Player {
                x: player_start.0,
                y: player_start.1,
                angle: 0.0,
                vel_forward: 0.0,
                vel_strafe: 0.0,
                vel_rot: 0.0,
                hp: PLAYER_MAX_HP,
                fire_cooldown: 0.0,
                muzzle: 0.0,
                damage_flash: 0.0,
                bob: 0.0,
            },
            imps,
            kills: 0,
            time: 0.0,
            hold: [0.0; Control::COUNT],
            release_aware: false,
            fire_queued: false,
            rng: super::assets::XorShift64::new(0x9E3779B97F4A7C15),
        }
    }

    /// Switch between the release-aware and timer movement models. Set once
    /// when the game opens, from the terminal's keyboard capability.
    pub fn set_release_aware(&mut self, release_aware: bool) {
        self.release_aware = release_aware;
    }

    pub fn total_imps(&self) -> u32 {
        self.imps.len() as u32
    }

    pub fn won(&self) -> bool {
        self.kills == self.total_imps()
    }

    pub fn dead(&self) -> bool {
        self.player.hp <= 0
    }

    /// Register a press/repeat event for `control`. In release-aware mode
    /// the control latches until [`Game::release`]; otherwise it stays held
    /// for [`HOLD_WINDOW`] seconds. Velocity is applied in `step`.
    pub fn press(&mut self, control: Control) {
        self.hold[control as usize] = if self.release_aware {
            f32::INFINITY
        } else {
            HOLD_WINDOW
        };
    }

    /// Register a key-release for `control` (release-aware mode only).
    pub fn release(&mut self, control: Control) {
        self.hold[control as usize] = 0.0;
    }

    /// Un-latch every control. Called on focus loss so a release event
    /// dropped while the window was unfocused can't latch movement forever.
    pub fn release_all(&mut self) {
        self.hold = [0.0; Control::COUNT];
    }

    /// Whether any movement control is currently held (latched or within
    /// the repeat-bridging window). Used to assert hold-clearing behavior.
    #[cfg(any(test, feature = "test-support"))]
    pub fn any_held(&self) -> bool {
        self.hold.iter().any(|&h| h > 0.0)
    }

    /// Request a pistol shot; fires on the next `step` if off cooldown.
    pub fn queue_fire(&mut self) {
        self.fire_queued = true;
    }

    /// Advance the world by `dt` seconds.
    pub fn step(&mut self, dt: f32) {
        self.time += dt;
        self.step_player(dt);
        if self.fire_queued {
            self.fire_queued = false;
            self.try_fire();
        }
        self.step_imps(dt);
    }

    fn step_player(&mut self, dt: f32) {
        // Age the hold timers, then read which controls are still held.
        for h in &mut self.hold {
            *h = (*h - dt).max(0.0);
        }
        let held = |c: Control| self.hold[c as usize] > 0.0;
        let axis = |pos: Control, neg: Control| (held(pos) as i32 - held(neg) as i32) as f32;

        // Steady target velocities from the held controls. The
        // forward/strafe pair is clamped to unit length so moving
        // diagonally isn't faster than moving straight.
        let mut fwd = axis(Control::Forward, Control::Back);
        let mut strafe = axis(Control::StrafeRight, Control::StrafeLeft);
        let mag = (fwd * fwd + strafe * strafe).sqrt();
        if mag > 1.0 {
            fwd /= mag;
            strafe /= mag;
        }
        let target_forward = fwd * MOVE_SPEED;
        let target_strafe = strafe * MOVE_SPEED;
        let target_rot = axis(Control::TurnRight, Control::TurnLeft) * TURN_SPEED;

        // Frame-rate-independent exponential smoothing toward the targets:
        // a constant target while held means no sawtooth, and a zero target
        // on release glides to a stop.
        let move_blend = 1.0 - (-dt / MOVE_ACCEL_TAU).exp();
        let turn_blend = 1.0 - (-dt / TURN_ACCEL_TAU).exp();

        let p = &mut self.player;
        p.vel_forward += (target_forward - p.vel_forward) * move_blend;
        p.vel_strafe += (target_strafe - p.vel_strafe) * move_blend;
        p.vel_rot += (target_rot - p.vel_rot) * turn_blend;

        // Integrate rotation, then position.
        p.angle += p.vel_rot * dt;
        let (dx, dy) = p.dir();
        // Strafe axis is dir rotated +90°.
        let (sx, sy) = (-dy, dx);
        let step_x = (dx * p.vel_forward + sx * p.vel_strafe) * dt;
        let step_y = (dy * p.vel_forward + sy * p.vel_strafe) * dt;

        // Axis-separated movement → slide along walls.
        if !self.map.blocked(p.x + step_x, p.y, PLAYER_RADIUS) {
            p.x += step_x;
        }
        if !self.map.blocked(p.x, p.y + step_y, PLAYER_RADIUS) {
            p.y += step_y;
        }

        p.bob += (p.vel_forward.abs() + p.vel_strafe.abs()) * dt;
        p.fire_cooldown = (p.fire_cooldown - dt).max(0.0);
        p.muzzle = (p.muzzle - dt).max(0.0);
        p.damage_flash = (p.damage_flash - dt * 1.8).max(0.0);
    }

    /// The live imp the pistol would hit right now: nearest one within
    /// [`HIT_WIDTH`] of the aim ray with clear line of sight. Shared by
    /// [`Game::try_fire`] and the renderer's crosshair feedback.
    pub fn target_in_crosshair(&self) -> Option<usize> {
        let (px, py) = (self.player.x, self.player.y);
        let (dx, dy) = self.player.dir();
        let mut best: Option<(usize, f32)> = None;
        for (i, imp) in self.imps.iter().enumerate() {
            if !imp.alive() {
                continue;
            }
            let (rx, ry) = (imp.x - px, imp.y - py);
            let along = rx * dx + ry * dy;
            if along <= 0.0 {
                continue;
            }
            let perp = (rx * dy - ry * dx).abs();
            if perp > HIT_WIDTH {
                continue;
            }
            if !self.map.los(px, py, imp.x, imp.y) {
                continue;
            }
            if best.is_none_or(|(_, d)| along < d) {
                best = Some((i, along));
            }
        }
        best.map(|(i, _)| i)
    }

    /// Hitscan pistol: damage the imp under the crosshair, if any.
    fn try_fire(&mut self) {
        if self.player.fire_cooldown > 0.0 {
            return;
        }
        self.player.fire_cooldown = FIRE_COOLDOWN;
        self.player.muzzle = MUZZLE_TIME;

        if let Some(i) = self.target_in_crosshair() {
            let damage = PISTOL_DAMAGE + (self.rng.next_f32() * 7.0) as i32;
            let imp = &mut self.imps[i];
            imp.hp -= damage;
            if imp.hp <= 0 {
                imp.state = ImpState::Dying { t: IMP_DEATH_TIME };
                self.kills += 1;
            } else {
                imp.state = ImpState::Pain { t: IMP_PAIN_TIME };
            }
        }
    }

    fn step_imps(&mut self, dt: f32) {
        let (px, py) = (self.player.x, self.player.y);
        let mut player_damage = 0;

        for i in 0..self.imps.len() {
            let (ix, iy, state) = {
                let imp = &self.imps[i];
                (imp.x, imp.y, imp.state)
            };
            // Corpses never act — skip the distance sqrt for them (late game
            // is mostly corpses).
            if matches!(state, ImpState::Dead) {
                continue;
            }
            let dist = ((px - ix).powi(2) + (py - iy).powi(2)).sqrt();

            match state {
                ImpState::Idle => {
                    if dist < IMP_SIGHT_RANGE && self.map.los(ix, iy, px, py) {
                        self.imps[i].state = ImpState::Chasing;
                    }
                }
                ImpState::Chasing => {
                    self.imps[i].attack_cooldown = (self.imps[i].attack_cooldown - dt).max(0.0);
                    if dist < IMP_MELEE_RANGE {
                        if self.imps[i].attack_cooldown <= 0.0 {
                            self.imps[i].state = ImpState::Attacking { t: IMP_WINDUP };
                        }
                    } else {
                        // The walk cycle only advances while actually
                        // moving, so an imp waiting out its attack
                        // cooldown doesn't march in place.
                        self.imps[i].anim += dt;
                        self.chase_step(i, px, py, dist, dt);
                    }
                }
                ImpState::Attacking { t } => {
                    let t = t - dt;
                    if t <= 0.0 {
                        // Bite lands if the player is still close.
                        let imp = &mut self.imps[i];
                        imp.state = ImpState::Chasing;
                        imp.attack_cooldown = IMP_ATTACK_COOLDOWN;
                        if dist < IMP_MELEE_RANGE * 1.25 {
                            player_damage += IMP_BITE_DAMAGE + (self.rng.next_f32() * 5.0) as i32;
                        }
                    } else {
                        self.imps[i].state = ImpState::Attacking { t };
                    }
                }
                ImpState::Pain { t } => {
                    let t = t - dt;
                    self.imps[i].state = if t <= 0.0 {
                        ImpState::Chasing
                    } else {
                        ImpState::Pain { t }
                    };
                }
                ImpState::Dying { t } => {
                    let t = t - dt;
                    self.imps[i].state = if t <= 0.0 {
                        ImpState::Dead
                    } else {
                        ImpState::Dying { t }
                    };
                }
                ImpState::Dead => {}
            }
        }

        if player_damage > 0 {
            self.player.hp = (self.player.hp - player_damage).max(0);
            self.player.damage_flash = 1.0;
        }
    }

    /// Move imp `i` toward the player with wall sliding and a small
    /// separation force from other live imps.
    fn chase_step(&mut self, i: usize, px: f32, py: f32, dist: f32, dt: f32) {
        let (ix, iy) = (self.imps[i].x, self.imps[i].y);
        let mut mx = (px - ix) / dist;
        let mut my = (py - iy) / dist;

        // Separation: push away from live imps closer than 0.7 tiles
        // (0.49 = 0.7²) so the pack doesn't collapse into one sprite.
        for (j, other) in self.imps.iter().enumerate() {
            if i == j || !other.alive() {
                continue;
            }
            let (ox, oy) = (ix - other.x, iy - other.y);
            let d2 = ox * ox + oy * oy;
            if d2 < 0.49 && d2 > 1e-6 {
                let d = d2.sqrt();
                mx += (ox / d) * 0.6;
                my += (oy / d) * 0.6;
            }
        }
        let mag = (mx * mx + my * my).sqrt().max(1e-4);
        let step = IMP_SPEED * dt;
        let (sx, sy) = (mx / mag * step, my / mag * step);

        let imp = &mut self.imps[i];
        if !self.map.blocked(imp.x + sx, imp.y, IMP_RADIUS) {
            imp.x += sx;
        }
        if !self.map.blocked(imp.x, imp.y + sy, IMP_RADIUS) {
            imp.y += sy;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_imp_spawns_reachable_from_player_start() {
        // Flood-fill walkable cells from the player start; every imp spawn
        // must be in the same connected component. This subsumes "spawns are
        // on floor tiles" (reachable cells are non-solid) and guards future
        // map edits against sealing a demon into an unreachable room.
        let game = Game::new();
        assert!(game.total_imps() >= 5, "want a meaningful demon count");
        let (w, h) = (game.map.w, game.map.h);
        let start = (game.player.x.floor() as i32, game.player.y.floor() as i32);
        let mut reachable = vec![false; w * h];
        let mut stack = vec![start];
        while let Some((x, y)) = stack.pop() {
            if game.map.solid(x, y) || reachable[y as usize * w + x as usize] {
                continue;
            }
            reachable[y as usize * w + x as usize] = true;
            stack.extend([(x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)]);
        }
        for imp in &game.imps {
            let (ix, iy) = (imp.x.floor() as usize, imp.y.floor() as usize);
            assert!(
                reachable[iy * w + ix],
                "imp spawn at ({ix}, {iy}) unreachable from player start"
            );
        }
    }

    #[test]
    fn map_border_is_solid() {
        let game = Game::new();
        for x in 0..game.map.w as i32 {
            assert!(game.map.solid(x, 0));
            assert!(game.map.solid(x, game.map.h as i32 - 1));
        }
        for y in 0..game.map.h as i32 {
            assert!(game.map.solid(0, y));
            assert!(game.map.solid(game.map.w as i32 - 1, y));
        }
    }

    #[test]
    fn player_cannot_walk_through_walls() {
        let mut game = Game::new();
        // Face straight up (-y) into the top border and push hard.
        game.player.angle = -std::f32::consts::FRAC_PI_2;
        for _ in 0..600 {
            game.press(Control::Forward);
            game.step(1.0 / 30.0);
        }
        assert!(
            game.player.y >= 1.0 + PLAYER_RADIUS - 1e-3,
            "clipped into border wall"
        );
    }

    #[test]
    fn los_blocked_by_walls_and_open_in_room() {
        let game = Game::new();
        let (px, py) = (game.player.x, game.player.y);
        // Opposite map corner is far behind many walls.
        assert!(!game.map.los(px, py, 20.5, 20.5));
        // A spot in the same starting room is visible.
        assert!(game.map.los(px, py, px + 1.0, py + 1.0));
    }

    #[test]
    fn shooting_an_imp_in_front_damages_and_eventually_kills() {
        let mut game = Game::new();
        // Plant a target two tiles in front of the player, clear LOS.
        let (dx, dy) = game.player.dir();
        let (tx, ty) = (game.player.x + dx * 2.0, game.player.y + dy * 2.0);
        game.imps[0].x = tx;
        game.imps[0].y = ty;
        game.imps[0].state = ImpState::Idle;

        let hp_before = game.imps[0].hp;
        game.queue_fire();
        game.step(0.016);
        assert!(game.imps[0].hp < hp_before, "first shot must connect");

        for _ in 0..20 {
            game.step(FIRE_COOLDOWN + 0.01); // let cooldown lapse
            game.queue_fire();
            game.step(0.016);
            if !game.imps[0].alive() {
                break;
            }
        }
        assert!(!game.imps[0].alive(), "imp should die after repeated hits");
        assert_eq!(game.kills, 1);
    }

    #[test]
    fn fire_respects_cooldown() {
        let mut game = Game::new();
        let (dx, dy) = game.player.dir();
        game.imps[0].x = game.player.x + dx * 2.0;
        game.imps[0].y = game.player.y + dy * 2.0;

        game.queue_fire();
        game.step(0.016);
        let hp_after_first = game.imps[0].hp;
        // Immediate second shot is swallowed by the cooldown.
        game.queue_fire();
        game.step(0.016);
        assert_eq!(game.imps[0].hp, hp_after_first);
    }

    #[test]
    fn imp_chases_and_bites_player() {
        let mut game = Game::new();
        // Keep only one imp and put it right next to the player.
        game.imps.truncate(1);
        game.imps[0].x = game.player.x + 1.5;
        game.imps[0].y = game.player.y;
        game.imps[0].state = ImpState::Idle;

        let mut bitten = false;
        for _ in 0..400 {
            game.step(1.0 / 30.0);
            if game.player.hp < PLAYER_MAX_HP {
                bitten = true;
                break;
            }
        }
        assert!(bitten, "imp should reach and bite the player");
        assert!(game.player.damage_flash > 0.0 || game.player.hp < PLAYER_MAX_HP);
    }

    #[test]
    fn win_condition_when_all_imps_dead() {
        let mut game = Game::new();
        for imp in &mut game.imps {
            imp.state = ImpState::Dead;
        }
        game.kills = game.total_imps();
        assert!(game.won());
        assert!(!game.dead());
    }

    #[test]
    fn movement_decays_to_rest() {
        // A single press (no further repeats) glides to a stop once the
        // hold window lapses.
        let mut game = Game::new();
        game.press(Control::Forward);
        game.press(Control::TurnRight);
        for _ in 0..120 {
            game.step(1.0 / 30.0);
        }
        assert!(game.player.vel_forward.abs() < 0.01);
        assert!(game.player.vel_rot.abs() < 0.01);
    }

    #[test]
    fn held_forward_reaches_and_sustains_target_speed() {
        // Holding forward (a press every frame) must converge to MOVE_SPEED
        // and then stay there — no sawtooth.
        let mut game = Game::new();
        // Aim down an open stretch so walls don't cap velocity.
        game.player.angle = std::f32::consts::FRAC_PI_2; // +y
        let dt = 1.0 / 30.0;
        for _ in 0..40 {
            game.press(Control::Forward);
            game.step(dt);
        }
        let speed = game.player.vel_forward;
        assert!(
            (speed - MOVE_SPEED).abs() < 0.05,
            "expected ~{MOVE_SPEED}, got {speed}"
        );
        // Sustained: the spread over the next second stays tiny.
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for _ in 0..30 {
            game.press(Control::Forward);
            game.step(dt);
            lo = lo.min(game.player.vel_forward);
            hi = hi.max(game.player.vel_forward);
        }
        assert!(hi - lo < 0.02, "velocity sawtoothed: [{lo}, {hi}]");
    }

    #[test]
    fn release_aware_supports_simultaneous_move_and_turn() {
        // The bug: on a Kitty-keyboard terminal, holding W + an arrow must
        // move AND turn. The terminal only auto-repeats the last key, so
        // forward gets a single press then nothing until release — latching
        // on press (release-aware) keeps it moving without repeats.
        let mut game = Game::new();
        game.set_release_aware(true);
        game.player.angle = 0.0; // facing +x
        game.press(Control::Forward);
        game.press(Control::TurnLeft);

        let angle0 = game.player.angle;
        for _ in 0..30 {
            game.step(1.0 / 30.0); // no further presses
        }
        assert!(
            game.player.vel_forward > 1.0,
            "forward must persist without repeats, got {}",
            game.player.vel_forward
        );
        assert!(
            game.player.angle < angle0,
            "should have turned left while moving"
        );

        // Releasing forward stops forward; turning continues.
        game.release(Control::Forward);
        for _ in 0..30 {
            game.step(1.0 / 30.0);
        }
        assert!(
            game.player.vel_forward.abs() < 0.2,
            "released forward should glide to rest, got {}",
            game.player.vel_forward
        );
        assert!(game.player.vel_rot.abs() > 0.1, "turn should still be held");
    }

    #[test]
    fn release_all_un_latches_movement() {
        // Safety valve for a release dropped on focus loss: latched keys
        // must all clear so the player doesn't walk forever.
        let mut game = Game::new();
        game.set_release_aware(true);
        game.press(Control::Forward);
        game.press(Control::TurnLeft);
        game.release_all();
        for _ in 0..30 {
            game.step(1.0 / 30.0);
        }
        assert!(game.player.vel_forward.abs() < 0.1);
        assert!(game.player.vel_rot.abs() < 0.1);
    }

    #[test]
    fn sustained_speed_is_independent_of_repeat_cadence() {
        // The whole point of the hold model: a slow key-repeat cadence must
        // reach the same sustained speed as a fast one (no stutter), as long
        // as repeats arrive within HOLD_WINDOW.
        fn sustained_speed(repeat_interval: f32) -> f32 {
            let mut game = Game::new();
            game.player.angle = std::f32::consts::FRAC_PI_2;
            let dt = 1.0 / 60.0;
            let mut since_repeat = repeat_interval; // press on the first frame
            // Run 2s of simulation, pressing every `repeat_interval`.
            for _ in 0..120 {
                since_repeat += dt;
                if since_repeat >= repeat_interval {
                    game.press(Control::Forward);
                    since_repeat = 0.0;
                }
                game.step(dt);
            }
            game.player.vel_forward
        }
        let fast = sustained_speed(0.03); // ~33 Hz
        let slow = sustained_speed(0.12); // ~8 Hz, still under HOLD_WINDOW
        assert!((fast - MOVE_SPEED).abs() < 0.1, "fast cadence: {fast}");
        assert!(
            (fast - slow).abs() < 0.25,
            "cadence changed sustained speed: fast={fast}, slow={slow}"
        );
    }
}
