use std::time::{Duration, Instant};

const RUN_DURATION: f32 = 2.8;
const RUN_ATTACK: f32 = 0.45;
const RUN_RELEASE: f32 = 0.5;
const RUN_HEAD: f32 = 4.0;
const RUN_TAIL: f32 = 18.0;
const SWEEP_STRENGTH: f32 = 0.45;
const COMPLETION_DURATION: f32 = 1.2;
const COMPLETION_ATTACK: f32 = 0.12;
const COMPLETION_STRENGTH: f32 = 0.7;
const GLOW_IGNITION: f32 = 0.6;
const GLOW_IGNITION_PEAK: f32 = 1.5;
const GLOW_IGNITION_ATTACK: f32 = 0.3;
const GLOW_RELEASE: f32 = 0.9;
const GLOW_DRAIN: f32 = 0.2;
const GLOW_RELEASE_PEAK: f32 = 1.25;
const GLOW_TAIL: f32 = 12.0;
const GLOW_STRENGTH: f32 = 0.6;
const SPRING_FREQUENCY: f32 = 2.0 * std::f32::consts::PI / (0.1 * 1.2);
pub const MARQUEE_DELAY: Duration = Duration::from_millis(600);
pub const MARQUEE_STEP: Duration = Duration::from_millis(80);

pub fn smootherstep(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    v * v * v * (v * (v * 6.0 - 15.0) + 10.0)
}

/// Eases with a constant-speed middle and 20% acceleration and deceleration ramps
fn coast(v: f32) -> f32 {
    let ramp = 0.2;
    let top = 1.0 / (1.0 - ramp);
    if v < ramp {
        0.5 * top * v * v / ramp
    } else if v < 1.0 - ramp {
        0.5 * top * ramp + top * (v - ramp)
    } else {
        1.0 - 0.5 * top * (1.0 - v) * (1.0 - v) / ramp
    }
}

fn since(t: Instant, now: Instant) -> f32 {
    now.saturating_duration_since(t).as_secs_f32()
}

fn one_shot(p: f32, attack: f32) -> f32 {
    if p < attack { smootherstep(p / attack) } else { 1.0 - smootherstep((p - attack) / (1.0 - attack)) }
}

fn band(i: f32, front: f32) -> f32 {
    let d = front - i;
    if d < 0.0 { smootherstep(1.0 + d / RUN_HEAD) } else { smootherstep(1.0 - d / RUN_TAIL) }
}

fn edge_glow(i: f32, tail: f32) -> f32 {
    smootherstep(1.0 - (i - 1.0).max(0.0) / tail)
}

#[derive(Default, Clone, Copy)]
pub struct Spring {
    pub value: f32,
    velocity: f32,
}

impl Spring {
    /// Moves towards the target with a critically damped spring, returning whether it is still moving
    pub fn step(&mut self, target: f32, dt: f32) -> bool {
        let offset = self.value - target;
        if offset.abs() < 0.002 && self.velocity.abs() < 0.002 {
            self.value = target;
            self.velocity = 0.0;
            return false;
        }
        let decay = (-SPRING_FREQUENCY * dt).exp();
        let carry = (self.velocity + SPRING_FREQUENCY * offset) * dt;
        self.value = target + (offset + carry) * decay;
        self.velocity = (self.velocity - SPRING_FREQUENCY * carry) * decay;
        true
    }
}

#[derive(Default)]
pub struct Fx {
    pub x: Spring,
    pub shift: Spring,
    pub width: Spring,
    pub selected: Spring,
    pub placed: bool,
    pub opening: bool,
    pub appear_at: Option<Instant>,
    busy: bool,
    unread: bool,
    run_start: Option<Instant>,
    run_stop: Option<Instant>,
    completion: Option<Instant>,
    glow_on: Option<Instant>,
    glow_off: Option<Instant>,
}

/// How strongly each effect tints one cell of a tab
pub struct CellFx {
    pub sweep: f32,
    pub accent: f32,
}

impl Fx {
    pub fn opening() -> Fx {
        Fx { opening: true, ..Default::default() }
    }

    pub fn opening_at(at: Instant) -> Fx {
        Fx { opening: true, appear_at: Some(at), ..Default::default() }
    }

    /// Records whether Claude is working, returning whether anything visible changed
    pub fn set_busy(&mut self, busy: bool, active: bool, now: Instant) -> bool {
        if busy == self.busy {
            return false;
        }
        self.busy = busy;
        if busy {
            self.run_start = Some(now);
            self.run_stop = None;
        } else {
            self.run_stop = Some(now);
            if !active {
                self.unread = true;
                self.completion = Some(now);
                self.glow_on = Some(now);
                self.glow_off = None;
            }
        }
        true
    }

    pub fn seen(&mut self, now: Instant) {
        if self.unread {
            self.unread = false;
            self.glow_off = Some(now);
        }
    }

    fn run_level(&self, now: Instant) -> f32 {
        let Some(start) = self.run_start else {
            return 0.0;
        };
        let attack = smootherstep(since(start, now) / RUN_ATTACK);
        match self.run_stop {
            Some(stop) => attack * (1.0 - smootherstep(since(stop, now) / RUN_RELEASE)),
            None => attack,
        }
    }

    pub fn animating(&self, now: Instant) -> bool {
        self.busy
            || self.run_stop.is_some_and(|t| since(t, now) < RUN_RELEASE)
            || self.completion.is_some_and(|t| since(t, now) < COMPLETION_DURATION)
            || self.glow_on.is_some_and(|t| self.unread && since(t, now) < GLOW_IGNITION)
            || self.glow_off.is_some_and(|t| since(t, now) < GLOW_RELEASE)
    }

    /// Works out every effect's strength across a tab of the given width
    pub fn cells(&self, width: usize, now: Instant) -> Vec<CellFx> {
        let w = width as f32;
        let run = self.run_level(now);
        let fronts: Vec<f32> = match (run > 0.0, self.run_start) {
            (true, Some(start)) => {
                let cycles = since(start, now) / RUN_DURATION;
                let span = w - 1.0 + RUN_TAIL + RUN_HEAD;
                let mut fronts = vec![-RUN_HEAD + coast(cycles % 1.0) * span];
                if cycles >= 0.5 {
                    fronts.push(-RUN_HEAD + coast((cycles + 0.5) % 1.0) * span);
                }
                fronts
            },
            _ => Vec::new(),
        };
        let completion = self
            .completion
            .map(|t| since(t, now) / COMPLETION_DURATION)
            .filter(|p| *p < 1.0)
            .map_or(0.0, |p| one_shot(p, COMPLETION_ATTACK));
        let rest = if self.unread {
            let p = self.glow_on.map_or(1.0, |t| since(t, now) / GLOW_IGNITION);
            if p >= 1.0 {
                1.0
            } else if p < GLOW_IGNITION_ATTACK {
                GLOW_IGNITION_PEAK * smootherstep(p / GLOW_IGNITION_ATTACK)
            } else {
                GLOW_IGNITION_PEAK
                    - (GLOW_IGNITION_PEAK - 1.0) * smootherstep((p - GLOW_IGNITION_ATTACK) / (1.0 - GLOW_IGNITION_ATTACK))
            }
        } else {
            0.0
        };
        let (drain, swell, spread) = match self.glow_off.map(|t| since(t, now)) {
            Some(t) if t < GLOW_RELEASE => {
                let p = t / GLOW_RELEASE;
                let swell = GLOW_RELEASE_PEAK * one_shot(p, 0.12);
                (1.0 - smootherstep(t / GLOW_DRAIN), swell, GLOW_TAIL + smootherstep(p) * w)
            },
            _ => (0.0, 0.0, GLOW_TAIL),
        };
        let tail = GLOW_TAIL.min(w);
        (0..width)
            .map(|i| {
                let i = i as f32;
                let sweep = fronts.iter().map(|&f| band(i, f)).fold(0.0, f32::max) * SWEEP_STRENGTH * run;
                let glow = (edge_glow(i, tail) * (rest + drain)).max(edge_glow(i, spread) * swell) * GLOW_STRENGTH;
                CellFx { sweep, accent: glow.max(completion * COMPLETION_STRENGTH).min(1.0) }
            })
            .collect()
    }
}
