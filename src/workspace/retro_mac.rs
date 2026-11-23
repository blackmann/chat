//! The empty-thread illustration: a line drawing of a compact Mac (128K / Plus
//! proportions, 9.6 × 13.6 × 10.9 in) with keyboard and mouse, matrix rain on its
//! screen, turning away from the pointer while it is over the drawing.
//!
//! GPUI has no 3D, so this is a small wireframe renderer. The model is a fixed set
//! of convex solids. Each frame they are projected, back faces are culled, and the
//! solids are painted far to near with their faces filled in the pane background,
//! which hides the lines behind them. Edges are stroked where two faces meet at an
//! angle or along the silhouette. Flat details (vents, slots) are line "decals"
//! drawn only while the face they sit on is turned towards the viewer.
//!
//! While the first prompt is typed, the keys for what was typed dip and glow green,
//! and each typed character drops down the next column in turn, leading it in green.

use std::{
    collections::HashMap,
    f32::consts::TAU,
    sync::LazyLock,
    time::{Duration, Instant},
};

use gpui::*;

use crate::theme::Palette;

/// Size of the drawing in the pane.
const WIDTH: f32 = 380.;
const HEIGHT: f32 = 330.;

/// Camera: orbit centre, distance, resting angles and how far the pointer turns it.
const CENTER: V3 = [0.15, 1.4, 0.7];
const DISTANCE: f32 = 11.;
const CAMERA: V3 = [0., 0., DISTANCE];
const REST_YAW: f32 = 0.38;
const REST_PITCH: f32 = 0.3;
const MAX_YAW: f32 = 0.6;
/// The pointer counts as over the drawing within this margin of it.
const HIT_PAD: f32 = 10.;
/// How often the rain moves on once the model has settled.
const TICK: Duration = Duration::from_millis(66);

/// Keys that aren't characters, as they appear in the model's key labels.
const DELETE: char = '\u{8}';
const SHIFT: char = '\u{e}';
/// How far a key travels, and how long it stays lit after a press.
const KEY_TRAVEL: f32 = 0.04;
const GLOW: f32 = 0.9;

const COLS: usize = 18;
const ROWS: usize = 15;
static GLYPHS: LazyLock<Vec<SharedString>> = LazyLock::new(|| {
    "ｱｲｳｴｵｶｷｸｹｺｻｼｽｾｿﾀﾁﾂﾃﾄﾅﾆﾇﾈﾉﾊﾋﾌﾍﾎﾏﾐﾑﾒﾓﾔﾕﾖﾗﾘﾙﾚﾛﾜﾝ0123456789:=*+-<>"
        .chars()
        .map(|c| SharedString::from(c.to_string()))
        .collect()
});

// ---- vectors -------------------------------------------------------------------

type V3 = [f32; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn mul(a: V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn normalize(a: V3) -> V3 {
    let len = dot(a, a).sqrt();
    if len == 0. { a } else { mul(a, 1. / len) }
}
fn lerp(a: V3, b: V3, t: f32) -> V3 {
    add(a, mul(sub(b, a), t))
}
fn centroid(points: impl Iterator<Item = V3>) -> V3 {
    let (sum, n) = points.fold(([0.; 3], 0), |(s, n), p| (add(s, p), n + 1));
    mul(sum, 1. / n.max(1) as f32)
}

// ---- model -------------------------------------------------------------------------

struct Edge {
    a: usize,
    b: usize,
    faces: [usize; 2],
    /// Stroked whenever either face shows; otherwise only on the silhouette.
    feature: bool,
}

/// Lines lying on a face, shown while that face is turned towards the viewer.
struct Decal {
    normal: V3,
    at: V3,
    lines: Vec<Vec<V3>>,
}

/// The lit glass, where the rain is drawn.
struct Screen {
    outline: Vec<V3>,
    /// Glass rectangle and corner radius, in model units, on the plane `z`.
    rect: [f32; 4],
    radius: f32,
    z: f32,
}

impl Screen {
    /// A point on the glass; `u` runs left to right, `v` top to bottom, both 0..1.
    fn at(&self, u: f32, v: f32) -> V3 {
        let [x0, y0, x1, y1] = self.rect;
        let m = 0.05;
        [x0 + m + u * (x1 - x0 - 2. * m), y1 - m - v * (y1 - y0 - 2. * m), self.z]
    }
    /// Whether a point on the glass lies inside its rounded corners, `inset` in from them.
    fn contains(&self, p: V3, inset: f32) -> bool {
        let [x0, y0, x1, y1] = self.rect;
        let r = self.radius;
        let (x, y) = (p[0], p[1]);
        if x < x0 + inset || x > x1 - inset || y < y0 + inset || y > y1 - inset {
            return false;
        }
        let cx = x.clamp(x0 + r, x1 - r);
        let cy = y.clamp(y0 + r, y1 - r);
        (x - cx).hypot(y - cy) <= r - inset
    }
}

struct Solid {
    verts: Vec<V3>,
    faces: Vec<Vec<usize>>,
    normals: Vec<V3>,
    edges: Vec<Edge>,
    center: V3,
    decals: Vec<Decal>,
    screen: Option<Screen>,
    /// Small parts (keys) that are ordered among themselves by depth.
    sortable: bool,
    /// The character a key types, for keys that light up.
    key: Option<char>,
}

struct Group {
    /// Groups are painted far to near by the depth of this point.
    reference: V3,
    solids: Vec<Solid>,
}

struct Model {
    groups: Vec<Group>,
    cables: Vec<Vec<V3>>,
}

fn solid(verts: Vec<V3>, faces: Vec<Vec<usize>>) -> Solid {
    let center = centroid(verts.iter().copied());
    let normals: Vec<V3> = faces
        .iter()
        .map(|f| {
            // Newell's method, then point it away from the centre.
            let mut n = [0.; 3];
            for i in 0..f.len() {
                let (a, b) = (verts[f[i]], verts[f[(i + 1) % f.len()]]);
                n[0] += (a[1] - b[1]) * (a[2] + b[2]);
                n[1] += (a[2] - b[2]) * (a[0] + b[0]);
                n[2] += (a[0] - b[0]) * (a[1] + b[1]);
            }
            let n = normalize(n);
            let mid = centroid(f.iter().map(|&i| verts[i]));
            if dot(n, sub(mid, center)) < 0. { mul(n, -1.) } else { n }
        })
        .collect();
    let mut shared: HashMap<(usize, usize), (usize, usize, Vec<usize>)> = HashMap::new();
    for (fi, f) in faces.iter().enumerate() {
        for k in 0..f.len() {
            let (a, b) = (f[k], f[(k + 1) % f.len()]);
            shared.entry((a.min(b), a.max(b))).or_insert((a, b, Vec::new())).2.push(fi);
        }
    }
    let crease = 22f32.to_radians().cos();
    let edges = shared
        .into_values()
        .map(|(a, b, fs)| {
            let faces = [fs[0], *fs.get(1).unwrap_or(&fs[0])];
            let feature = fs.len() < 2 || dot(normals[faces[0]], normals[faces[1]]) < crease;
            Edge { a, b, faces, feature }
        })
        .collect();
    Solid { verts, faces, normals, edges, center, decals: Vec::new(), screen: None, sortable: false, key: None }
}

/// Join rings of equal length into a closed solid, capped at both ends.
fn loft(rings: Vec<Vec<V3>>) -> Solid {
    let n = rings[0].len();
    let last = rings.len() - 1;
    let mut faces = vec![(0..n).collect::<Vec<_>>(), (0..n).map(|i| last * n + i).collect()];
    for r in 0..last {
        for i in 0..n {
            let j = (i + 1) % n;
            let (a, b) = (r * n, (r + 1) * n);
            faces.push(vec![a + i, a + j, b + j, b + i]);
        }
    }
    solid(rings.concat(), faces)
}

/// Extrude an (x, y) outline along z.
fn extrude_z(outline: &[[f32; 2]], from: f32, to: f32) -> Solid {
    loft(vec![at_z(outline, from), at_z(outline, to)])
}

fn prism(bottom: [V3; 4], top: [V3; 4]) -> Solid {
    loft(vec![bottom.to_vec(), top.to_vec()])
}

fn round_rect([x0, y0, x1, y1]: [f32; 4], r: f32, seg: usize) -> Vec<[f32; 2]> {
    if r <= 0. {
        return vec![[x1, y1], [x0, y1], [x0, y0], [x1, y0]];
    }
    let corners: [(f32, f32, f32); 4] = [(x1 - r, y1 - r, 0.), (x0 + r, y1 - r, 90.), (x0 + r, y0 + r, 180.), (x1 - r, y0 + r, 270.)];
    let mut points = Vec::with_capacity(4 * (seg + 1));
    for (cx, cy, start) in corners {
        for i in 0..=seg {
            let a = (start + 90. * i as f32 / seg as f32).to_radians();
            points.push([cx + r * a.cos(), cy + r * a.sin()]);
        }
    }
    points
}

fn at_z(outline: &[[f32; 2]], z: f32) -> Vec<V3> {
    outline.iter().map(|&[x, y]| [x, y, z]).collect()
}

/// A closed rounded rectangle drawn on the plane `z`.
fn outline_z(rect: [f32; 4], r: f32, z: f32, seg: usize) -> Vec<V3> {
    let mut line = at_z(&round_rect(rect, r, seg), z);
    line.push(line[0]);
    line
}

fn bezier(p0: V3, p1: V3, p2: V3, p3: V3, steps: usize) -> Vec<V3> {
    (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            let u = 1. - t;
            add(add(mul(p0, u * u * u), mul(p1, 3. * u * u * t)), add(mul(p2, 3. * u * t * t), mul(p3, t * t * t)))
        })
        .collect()
}

static MODEL: LazyLock<Model> = LazyLock::new(build);

/// Units are 4 in. y is up, +z points at the viewer.
fn build() -> Model {
    let mut groups = Vec::new();

    // The computer.
    {
        const ZF: f32 = 1.25; // front of the bezel
        const ZS: f32 = 0.65; // seam between bezel and rear case
        const ZB: f32 = ZF - 2.725; // back
        const FOOT: f32 = 0.45; // height of the recessed chin
        const SEG: usize = 4;

        // Rear case: down to the floor, narrower at the back, top sloping down.
        let mut rear = loft(vec![
            at_z(&round_rect([-1.19, 0., 1.19, 3.39], 0.08, SEG), ZS),
            at_z(&round_rect([-1.1, 0., 1.1, 3.27], 0.26, SEG), ZB),
        ]);
        let top_y = |z: f32| 3.39 + (z - ZS) / (ZB - ZS) * (3.27 - 3.39) + 0.002;
        let side_x = |z: f32| 1.19 + (z - ZS) / (ZB - ZS) * (1.1 - 1.19) + 0.002;
        // Carry-handle channel in the top, open at the back.
        let (hz, back) = (-0.55, ZB + 0.05);
        let channel = |w: f32, front: f32| vec![[-w, top_y(back), back], [-w, top_y(front), front], [w, top_y(front), front], [w, top_y(back), back]];
        rear.decals.push(Decal { normal: [0., 1., -0.04], at: [0., top_y(-0.4), -0.4], lines: vec![channel(0.5, hz), channel(0.42, hz + 0.08)] });
        // Vents low on each side, towards the back.
        for sign in [1., -1.] {
            let (za, zb) = (-1.25, -0.45);
            let lines = (0..8)
                .map(|i| {
                    let y = 0.52 + i as f32 * 0.055;
                    vec![[sign * side_x(za), y, za], [sign * side_x(zb), y, zb]]
                })
                .collect();
            rear.decals.push(Decal { normal: [sign, 0., 0.04], at: [sign * side_x(-0.8), 0.7, -0.8], lines });
        }

        // Recessed chin: brightness symbol on the left, keyboard port on the right.
        let zc = ZF - 0.1;
        let mut chin = extrude_z(&round_rect([-1.16, 0., 1.16, FOOT], 0., SEG), ZS, zc);
        let sun = (0..=16)
            .map(|i| {
                let a = i as f32 / 16. * TAU;
                [-0.8 + a.cos() * 0.035, 0.24 + a.sin() * 0.035, zc]
            })
            .collect();
        chin.decals.push(Decal {
            normal: [0., 0., 1.],
            at: [0., 0.2, zc],
            lines: vec![sun, outline_z([0.78, 0.13, 0.94, 0.33], 0., zc, 0), outline_z([0.82, 0.18, 0.9, 0.28], 0., zc, 0)],
        });

        // Front bezel, its front edge chamfered on the top and sides.
        let ch = 0.06;
        let body = round_rect([-1.2, FOOT, 1.2, 3.4], 0.08, SEG);
        let mut bezel = loft(vec![
            at_z(&body, ZS),
            at_z(&body, ZF - ch),
            at_z(&round_rect([-1.2 + ch, FOOT, 1.2 - ch, 3.4 - ch], 0.05, SEG), ZF),
        ]);
        // A wide opening whose walls slope back to the tube's frame, deeper at the bottom.
        let zi = ZF - 0.16;
        let opening = [-0.98, 1.36, 0.98, 2.96];
        let frame = [-0.9, 1.46, 0.9, 2.91];
        let (glass, glass_r) = ([-0.83, 1.52, 0.83, 2.86], 0.14);
        let corners = |[x0, y0, x1, y1]: [f32; 4], z: f32| [[x1, y1, z], [x0, y1, z], [x0, y0, z], [x1, y0, z]];
        let (oc, fc) = (corners(opening, ZF), corners(frame, zi));
        let mut lines = vec![outline_z(opening, 0.02, ZF, 2), outline_z(frame, 0.03, zi, 2), outline_z(glass, glass_r, zi, 5)];
        lines.extend((0..4).map(|i| vec![oc[i], fc[i]]));
        lines.extend([
            outline_z([-0.05, 0.9, 0.6, 0.95], 0., ZF, 0), // floppy slot
            outline_z([0.58, 0.85, 0.9, 1.0], 0., ZF, 0),  // eject housing
            outline_z([0.66, 0.89, 0.84, 0.94], 0., ZF, 0),
            outline_z([-0.97, 0.63, -0.83, 0.77], 0.025, ZF, 2), // badge
        ]);
        bezel.decals.push(Decal { normal: [0., 0., 1.], at: [0., 1.5, ZF], lines });
        let z = zi + 0.004;
        bezel.screen = Some(Screen { outline: at_z(&round_rect(glass, glass_r, 5), z), rect: glass, radius: glass_r, z });

        groups.push(Group { reference: [0., 1.7, -0.1], solids: vec![rear, chin, bezel] });
    }

    // The keyboard, with its keys standing on the sloped top.
    {
        let (x0, x1, z0, z1, hb, hf) = (-1.72, 1.52, 1.72, 2.76, 0.2, 0.09);
        let h = |z: f32| hb + (z - z0) / (z1 - z0) * (hf - hb);
        let mut solids = vec![prism(
            [[x0, 0., z0], [x1, 0., z0], [x1, 0., z1], [x0, 0., z1]],
            [[x0, hb, z0], [x1, hb, z0], [x1, hf, z1], [x0, hf, z1]],
        )];
        let (margin, gap_block) = (0.09, 0.1);
        let u = (x1 - x0 - 2. * margin - gap_block) / 19.;
        let main: [&[f32]; 5] = [
            &[1., 1., 1., 1., 1., 1., 1., 1., 1., 1., 1., 1., 1., 2.],
            &[1.5, 1., 1., 1., 1., 1., 1., 1., 1., 1., 1., 1., 1., 1.5],
            &[1.75, 1., 1., 1., 1., 1., 1., 1., 1., 1., 1., 1., 2.25],
            &[2.25, 1., 1., 1., 1., 1., 1., 1., 1., 1., 1., 2.75],
            &[1.25, 1.25, 1.25, 6.25, 1.25, 1.25, 1.25, 1.25],
        ];
        // Which character each main-block key stands for, so typing can find it.
        // Nothing is drawn on the caps. '\0' marks keys that never light.
        let typed_by: [&str; 5] = ["`1234567890-=\u{8}", "\tqwertyuiop[]\\", "\0asdfghjkl;'\n", "\u{e}zxcvbnm,./\0", "\0\0\0 \0\0\0\0"];
        let pad: [&[f32]; 5] = [&[1., 1., 1., 1.], &[1., 1., 1., 1.], &[1., 1., 1., 1.], &[1., 1., 1., 1.], &[2., 1., 1.]];
        let (g, inset, kh) = (0.016, 0.016, 0.05);
        let mut key = |a: f32, b: f32, c: f32, d: f32, label: Option<char>| {
            let (a, b, c, d) = (a + g, b - g, c + g, d - g);
            let mut s = prism(
                [[a, h(c), c], [b, h(c), c], [b, h(d), d], [a, h(d), d]],
                [
                    [a + inset, h(c) + kh, c + inset],
                    [b - inset, h(c) + kh, c + inset],
                    [b - inset, h(d) + kh, d - inset],
                    [a + inset, h(d) + kh, d - inset],
                ],
            );
            s.sortable = true;
            s.key = label;
            solids.push(s);
        };
        for (rows, start, is_main) in [(main, x0 + margin, true), (pad, x0 + margin + 15. * u + gap_block, false)] {
            for (r, row) in rows.iter().enumerate() {
                let zc = z0 + 0.12 + r as f32 * u;
                let mut x = start;
                let mut row_keys = typed_by[r].chars();
                for w in row.iter() {
                    let label = if is_main { row_keys.next().filter(|&c| c != '\0') } else { None };
                    key(x, x + w * u, zc, zc + u, label);
                    x += w * u;
                }
            }
        }
        groups.push(Group { reference: [-0.1, 0.1, 2.24], solids });
    }

    // The mouse, its button at the far end.
    {
        let (a, b, c, d, i, hb, hf) = (1.78, 2.18, 2.05, 2.68, 0.04, 0.13, 0.1);
        let mut mouse = prism(
            [[a, 0., c], [b, 0., c], [b, 0., d], [a, 0., d]],
            [[a + i, hb, c + i], [b - i, hb, c + i], [b - i, hf, d - i], [a + i, hf, d - i]],
        );
        let y = |z: f32| hb + (z - c - i) / (d - c - 2. * i) * (hf - hb) + 0.001;
        let (b0, b1) = (c + 0.08, c + 0.26);
        mouse.decals.push(Decal {
            normal: [0., 1., 0.1],
            at: [(a + b) / 2., hb, c + 0.2],
            lines: vec![vec![[a + 0.08, y(b0), b0], [b - 0.08, y(b0), b0], [b - 0.08, y(b1), b1], [a + 0.08, y(b1), b1], [a + 0.08, y(b0), b0]]],
        });
        groups.push(Group { reference: [1.98, 0.05, 2.36], solids: vec![mouse] });
    }

    // Cables lie under everything and are painted first.
    let mut cables = Vec::new();
    {
        // Coiled keyboard cable out of the chin's port.
        let (from, to) = ([0.86, 0.22, 1.15], [0.95, 0.05, 1.6]);
        let dir = normalize(sub(to, from));
        let side = normalize(cross(dir, [0., 1., 0.]));
        let up = cross(side, dir);
        let (turns, steps) = (11., 154);
        cables.push(
            (0..=steps)
                .map(|k| {
                    let t = k as f32 / steps as f32;
                    let a = t * turns * TAU;
                    add(lerp(from, to, t), add(mul(side, a.cos() * 0.045), mul(up, a.sin() * 0.045)))
                })
                .collect(),
        );
        cables.push(bezier(to, [1.0, 0., 1.7], [0.9, 0.05, 1.7], [0.8, 0.1, 1.72], 24));
        cables.push(bezier([1.12, 0.3, -1.2], [2.0, 0., -1.0], [2.6, 0., 1.1], [1.98, 0.06, 2.05], 40));
    }

    Model { groups, cables }
}

// ---- camera ------------------------------------------------------------------------

struct Camera {
    cos_yaw: f32,
    sin_yaw: f32,
    cos_pitch: f32,
    sin_pitch: f32,
    focal: f32,
    origin: Point<Pixels>,
}

impl Camera {
    fn new(yaw: f32, pitch: f32, bounds: Bounds<Pixels>) -> Self {
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        Self {
            cos_yaw: yaw.cos(),
            sin_yaw: yaw.sin(),
            cos_pitch: pitch.cos(),
            sin_pitch: pitch.sin(),
            focal: (w * 1.75).min(h * 2.15),
            origin: bounds.center(),
        }
    }
    fn turn(&self, p: V3) -> V3 {
        let x = p[0] * self.cos_yaw + p[2] * self.sin_yaw;
        let z = -p[0] * self.sin_yaw + p[2] * self.cos_yaw;
        [x, p[1] * self.cos_pitch - z * self.sin_pitch, p[1] * self.sin_pitch + z * self.cos_pitch]
    }
    fn view(&self, p: V3) -> V3 {
        self.turn(sub(p, CENTER))
    }
    fn project(&self, v: V3) -> Point<Pixels> {
        let d = DISTANCE - v[2];
        point(self.origin.x + px(self.focal * v[0] / d), self.origin.y - px(self.focal * v[1] / d))
    }
    fn point(&self, p: V3) -> Point<Pixels> {
        self.project(self.view(p))
    }
    fn faces_viewer(&self, normal: V3, at: V3) -> bool {
        dot(self.turn(normal), sub(CAMERA, self.view(at))) > 0.
    }
}

// ---- state -----------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Rain {
    head: f32,
    speed: f32,
    len: f32,
}

/// What the screen and keyboard show this frame.
struct Frame {
    rain: Vec<Rain>,
    cells: Vec<usize>,
    tips: Vec<Option<SharedString>>,
    /// Lit keys and how long ago each was pressed, in seconds.
    presses: Vec<(char, f32)>,
}

struct Art {
    yaw: f32,
    pitch: f32,
    /// Where the pointer is over the drawing, -1..1 across and down.
    pointer: Option<(f32, f32)>,
    last: Instant,
    seed: u64,
    rain: Vec<Rain>,
    cells: Vec<usize>,
    swaps: f32,
    /// The typed character leading each column's current drop, if any. It goes
    /// when that drop leaves the glass.
    tips: Vec<Option<SharedString>>,
    /// The column the next typed character drops down.
    column: usize,
    ticking: bool,
    /// The composer text last seen, to tell what was just typed.
    typed: Option<String>,
    presses: Vec<(char, Instant)>,
}

impl Art {
    fn new() -> Self {
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0x9e37_79b9, |d| d.as_nanos() as u64) | 1;
        let mut art = Self {
            yaw: REST_YAW,
            pitch: REST_PITCH,
            pointer: None,
            last: Instant::now(),
            seed,
            rain: Vec::new(),
            cells: Vec::new(),
            swaps: 0.,
            tips: vec![None; COLS],
            column: 0,
            ticking: false,
            typed: None,
            presses: Vec::new(),
        };
        art.cells = (0..COLS * ROWS).map(|_| art.glyph()).collect();
        art.rain = (0..COLS).map(|_| art.drop(true)).collect();
        art
    }

    fn random(&mut self) -> f32 {
        // xorshift64*
        self.seed ^= self.seed >> 12;
        self.seed ^= self.seed << 25;
        self.seed ^= self.seed >> 27;
        (self.seed.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 40) as f32 / (1u64 << 24) as f32
    }
    fn glyph(&mut self) -> usize {
        ((self.random() * GLYPHS.len() as f32) as usize).min(GLYPHS.len() - 1)
    }
    fn drop(&mut self, first: bool) -> Rain {
        let rows = ROWS as f32;
        let head = if first { self.random() * rows * 1.6 - rows * 0.6 } else { -self.random() * 10. };
        Rain { head, speed: 5. + self.random() * 9., len: 5. + (self.random() * 12.).floor() }
    }

    /// Press the keys for whatever changed in the composer since last time.
    /// The first text seen (say, a restored draft) presses nothing.
    fn observe(&mut self, text: &str) {
        let Some(previous) = self.typed.replace(text.to_string()) else { return };
        if previous == text {
            return;
        }
        let now = Instant::now();
        let (before, after) = (previous.chars().count(), text.chars().count());
        if after <= before {
            self.press(DELETE, now);
            return;
        }
        // Characters went in where the two texts start to differ. A paste lights
        // only its last few.
        let common = previous.chars().zip(text.chars()).take_while(|(a, b)| a == b).count();
        let inserted: Vec<char> = text.chars().skip(common).take(after - before).collect();
        for &c in inserted.iter().rev().take(COLS).rev() {
            self.drop_typed(c);
        }
        for &c in inserted.iter().rev().take(6) {
            let (key, shifted) = key_for(c);
            self.press(key, now);
            if shifted {
                self.press(SHIFT, now);
            }
        }
    }
    /// Start a fresh drop down the next column, led by `c`. A blank still takes
    /// its turn, so words keep their gaps.
    fn drop_typed(&mut self, c: char) {
        let column = self.column;
        self.column = (column + 1) % COLS;
        self.rain[column] = Rain { head: 0., ..self.drop(false) };
        self.tips[column] = (!c.is_whitespace()).then(|| SharedString::from(c.to_string()));
    }
    fn press(&mut self, key: char, at: Instant) {
        self.presses.retain(|(k, _)| *k != key);
        self.presses.push((key, at));
    }

    /// Move time on; true while something is still moving.
    fn advance(&mut self) -> bool {
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f32().min(0.05);
        self.last = now;

        let (yaw, pitch) = match self.pointer {
            // Pointer to the right turns it to the left, and the other way round.
            Some((x, y)) => (-x * MAX_YAW, REST_PITCH + y * 0.1),
            None => (REST_YAW, REST_PITCH),
        };
        let k = 1. - (-dt * 6.).exp();
        self.yaw += (yaw - self.yaw) * k;
        self.pitch += (pitch - self.pitch) * k;
        let turning = (yaw - self.yaw).abs() > 0.0005 || (pitch - self.pitch).abs() > 0.0005;
        if !turning {
            (self.yaw, self.pitch) = (yaw, pitch);
        }

        for i in 0..COLS {
            self.rain[i].head += self.rain[i].speed * dt;
            if self.rain[i].head - self.rain[i].len > ROWS as f32 {
                self.rain[i] = self.drop(false);
                self.tips[i] = None;
            }
        }
        self.swaps += (COLS * ROWS) as f32 * 0.6 * dt;
        while self.swaps >= 1. {
            self.swaps -= 1.;
            let cell = ((self.random() * self.cells.len() as f32) as usize).min(self.cells.len() - 1);
            self.cells[cell] = self.glyph();
        }
        let now = Instant::now();
        self.presses.retain(|(_, at)| now.duration_since(*at).as_secs_f32() < GLOW);
        turning || !self.presses.is_empty()
    }
}

/// The key that types `c`, and whether Shift goes with it.
fn key_for(c: char) -> (char, bool) {
    const SHIFTED: &str = "~!@#$%^&*()_+{}|:\"<>?";
    const BASE: &str = "`1234567890-=[]\\;',./";
    if c.is_ascii_uppercase() {
        return (c.to_ascii_lowercase(), true);
    }
    match SHIFTED.chars().position(|s| s == c) {
        Some(i) => (BASE.chars().nth(i).unwrap_or(c), true),
        None => (c, false),
    }
}

/// How far down a key is (0..1) and how brightly it glows (0..1), `t` seconds after a press.
fn key_press(t: f32) -> (f32, f32) {
    let down = if t < 0.05 { t / 0.05 } else { (1. - (t - 0.05) / 0.15).max(0.) };
    let glow = if t < 0.1 { 1. } else { (1. - (t - 0.1) / (GLOW - 0.1)).max(0.) };
    (down, glow * glow)
}

// ---- element ---------------------------------------------------------------------

/// The drawing, sized for the middle of an empty thread.
#[derive(IntoElement)]
pub struct RetroMac {
    colors: Palette,
    typed: SharedString,
}

impl RetroMac {
    /// `typed` is the composer's text: what changes in it is pressed on the keyboard.
    pub fn new(colors: Palette, typed: impl Into<SharedString>) -> Self {
        Self { colors, typed: typed.into() }
    }
}

impl RenderOnce for RetroMac {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state("retro-mac", cx, |_, _| Art::new());
        let (colors, typed) = (self.colors, self.typed);
        state.update(cx, |s, _| s.observe(&typed));
        canvas(|_, _, _| {}, move |bounds, _, window, cx| paint(&state, bounds, colors, window, cx))
            .w(px(WIDTH))
            .h(px(HEIGHT))
            .max_w_full()
            .flex_shrink()
    }
}

fn paint(state: &Entity<Art>, bounds: Bounds<Pixels>, colors: Palette, window: &mut Window, cx: &mut App) {
    // The pointer counts while it is over the drawing as it rests, so the area
    // doesn't move under it as the model turns.
    let hit = rest_bounds(bounds);
    if !window.is_window_hovered() {
        state.update(cx, |s, _| s.pointer = None);
    }
    let turning = state.update(cx, |s, _| s.advance());

    let (camera, frame) = {
        let art = state.read(cx);
        let now = Instant::now();
        let presses = art.presses.iter().map(|&(k, at)| (k, now.duration_since(at).as_secs_f32())).collect();
        (Camera::new(art.yaw, art.pitch, bounds), Frame { rain: art.rain.clone(), cells: art.cells.clone(), tips: art.tips.clone(), presses })
    };
    let model = &*MODEL;
    let line = Hsla::from(rgb(colors.muted));
    let background = Hsla::from(rgb(colors.background));

    stroke(window, model.cables.iter().map(|l| l.iter().map(|&p| camera.point(p))), line);

    let mut groups: Vec<&Group> = model.groups.iter().collect();
    groups.sort_by(|a, b| camera.view(a.reference)[2].total_cmp(&camera.view(b.reference)[2]));
    for group in groups {
        let mut keys: Vec<&Solid> = group.solids.iter().filter(|s| s.sortable).collect();
        keys.sort_by(|a, b| camera.view(a.center)[2].total_cmp(&camera.view(b.center)[2]));
        for solid in group.solids.iter().filter(|s| !s.sortable).chain(keys) {
            paint_solid(solid, &camera, &frame, colors, line, background, window, cx);
        }
    }

    let weak = state.downgrade();
    window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
        if !phase.bubble() {
            return;
        }
        let pointer = hit.contains(&e.position).then(|| {
            let x = f32::from(e.position.x - hit.left()) / f32::from(hit.size.width) * 2. - 1.;
            let y = f32::from(e.position.y - hit.top()) / f32::from(hit.size.height) * 2. - 1.;
            (x, y)
        });
        let _ = weak.update(cx, |s, cx| {
            if pointer.is_some() || s.pointer.is_some() {
                s.pointer = pointer;
                cx.notify();
            }
        });
    });

    if turning {
        window.request_animation_frame();
    } else {
        schedule(state, window, cx);
    }
}

/// Redraw for the rain after a tick, holding off while the window is in the background.
fn schedule(state: &Entity<Art>, window: &mut Window, cx: &mut App) {
    if state.update(cx, |s, _| std::mem::replace(&mut s.ticking, true)) {
        return;
    }
    let weak = state.downgrade();
    window
        .spawn(cx, async move |cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if weak.upgrade().is_none() {
                    return;
                }
                let active = cx.update(|window, _| window.is_window_active()).unwrap_or(false);
                if active {
                    let _ = weak.update(cx, |s, cx| {
                        s.ticking = false;
                        cx.notify();
                    });
                    return;
                }
            }
        })
        .detach();
}

/// The drawing's on-screen extent at rest, padded.
fn rest_bounds(bounds: Bounds<Pixels>) -> Bounds<Pixels> {
    let camera = Camera::new(REST_YAW, REST_PITCH, bounds);
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for solid in MODEL.groups.iter().flat_map(|g| &g.solids) {
        for &v in &solid.verts {
            let p = camera.point(v);
            let (x, y) = (f32::from(p.x), f32::from(p.y));
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
    }
    Bounds::from_corners(point(px(x0 - HIT_PAD), px(y0 - HIT_PAD)), point(px(x1 + HIT_PAD), px(y1 + HIT_PAD)))
}

#[allow(clippy::too_many_arguments)]
fn paint_solid(solid: &Solid, camera: &Camera, frame: &Frame, colors: Palette, line: Hsla, background: Hsla, window: &mut Window, cx: &mut App) {
    let (down, glow) = solid
        .key
        .and_then(|k| frame.presses.iter().find(|(p, _)| *p == k))
        .map_or((0., 0.), |&(_, t)| key_press(t));
    let dip = [0., -down * KEY_TRAVEL, 0.];
    let viewed: Vec<V3> = solid.verts.iter().map(|&v| camera.view(add(v, dip))).collect();
    let points: Vec<Point<Pixels>> = viewed.iter().map(|&v| camera.project(v)).collect();
    let shown: Vec<bool> = solid
        .faces
        .iter()
        .zip(&solid.normals)
        .map(|(f, &n)| dot(camera.turn(n), sub(CAMERA, viewed[f[0]])) > 0.)
        .collect();

    // Faces in the background colour hide whatever was drawn behind them.
    let mut faces = PathBuilder::fill();
    let mut any = false;
    for (f, _) in solid.faces.iter().zip(&shown).filter(|(_, s)| **s) {
        faces.move_to(points[f[0]]);
        for &i in &f[1..] {
            faces.line_to(points[i]);
        }
        faces.close();
        any = true;
    }
    if any && let Ok(path) = faces.build() {
        window.paint_path(path, background);
    }
    // A lit key's top (the second face of a prism) fills green.
    let green = Hsla::from(rgb(colors.green));
    if glow > 0. && shown.get(1) == Some(&true) {
        let mut top = PathBuilder::fill();
        let f = &solid.faces[1];
        top.move_to(points[f[0]]);
        f[1..].iter().for_each(|&i| top.line_to(points[i]));
        top.close();
        if let Ok(path) = top.build() {
            window.paint_path(path, green.opacity(0.55 * glow));
        }
    }

    if let Some(screen) = &solid.screen {
        paint_screen(screen, camera, frame, colors, window, cx);
    }

    let edges: Vec<[Point<Pixels>; 2]> = solid
        .edges
        .iter()
        .filter(|e| {
            let (a, b) = (shown[e.faces[0]], shown[e.faces[1]]);
            (a || b) && (e.feature || a != b)
        })
        .map(|e| [points[e.a], points[e.b]])
        .collect();
    stroke(window, edges.iter().map(|e| e.iter().copied()), line);
    if glow > 0. {
        stroke(window, edges.iter().map(|e| e.iter().copied()), green.opacity(glow));
    }

    for decal in &solid.decals {
        if camera.faces_viewer(decal.normal, decal.at) {
            stroke(window, decal.lines.iter().map(|l| l.iter().map(|&p| camera.point(p))), line);
        }
    }
}

fn paint_screen(screen: &Screen, camera: &Camera, frame: &Frame, colors: Palette, window: &mut Window, cx: &mut App) {
    if !camera.faces_viewer([0., 0., 1.], screen.at(0.5, 0.5)) {
        return;
    }
    // Light rain on black glass in the dark theme, ink on pale glass in the light one.
    let (glass, trail, head) = if colors.dark { (0x08090a, 0xc8cdd7, 0xffffff) } else { (0xececea, 0x46484e, 0x0a0a0c) };
    let mut outline = PathBuilder::fill();
    let mut points = screen.outline.iter().map(|&p| camera.point(p));
    if let Some(first) = points.next() {
        outline.move_to(first);
        points.for_each(|p| outline.line_to(p));
        outline.close();
        if let Ok(path) = outline.build() {
            window.paint_path(path, rgb(glass));
        }
    }

    // One glyph per lit cell, sized to the projected row height. Rounding the size
    // lets shaped glyphs be reused from frame to frame.
    let (a, b) = (camera.point(screen.at(0.5, 0.5)), camera.point(screen.at(0.5, 0.5 + 1. / ROWS as f32)));
    let row = f32::from(b.y - a.y).hypot(f32::from(b.x - a.x));
    let size = px((row * 1.05 * 4.).round() / 4.);
    if size < px(3.) {
        return;
    }
    let mut run = window.text_style().to_run(0);
    run.font.family = crate::theme::mono();
    let (trail, head, typed) = (Hsla::from(rgb(trail)), Hsla::from(rgb(head)), Hsla::from(rgb(colors.green)));
    for (c, rain) in frame.rain.iter().enumerate() {
        for r in 0..ROWS {
            let dist = rain.head - r as f32;
            if dist < 0. || dist > rain.len {
                continue;
            }
            let at = screen.at((c as f32 + 0.5) / COLS as f32, (r as f32 + 0.5) / ROWS as f32);
            if !screen.contains(at, 0.03) {
                continue;
            }
            let fade = 1. - dist / rain.len;
            let tip = frame.tips[c].as_ref().filter(|_| dist < 1.);
            let color = match tip {
                Some(_) => typed,
                None if dist < 1. => head,
                None => trail.opacity(fade * fade * 0.9),
            };
            let text = tip.cloned().unwrap_or_else(|| GLYPHS[frame.cells[c * ROWS + r]].clone());
            let runs = [TextRun { len: text.len(), color, ..run.clone() }];
            let shaped = window.text_system().shape_line(text, size, &runs, None);
            let center = camera.point(at);
            let origin = point(center.x - shaped.width / 2., center.y - size / 2.);
            let _ = shaped.paint(origin, size, window, cx);
        }
    }
}

/// Stroke polylines as one hairline path.
fn stroke<L: Iterator<Item = Point<Pixels>>>(window: &mut Window, lines: impl Iterator<Item = L>, color: Hsla) {
    let mut path = PathBuilder::stroke(px(1.));
    let mut any = false;
    for mut line in lines {
        let Some(first) = line.next() else { continue };
        path.move_to(first);
        for p in line {
            path.line_to(p);
            any = true;
        }
    }
    if any && let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

#[cfg(test)]
mod tests {
    use super::{Art, COLS, DELETE, MODEL, ROWS, SHIFT, key_for};

    #[test]
    fn shifted_characters_press_their_base_key_and_shift() {
        assert_eq!(key_for('a'), ('a', false));
        assert_eq!(key_for('A'), ('a', true));
        assert_eq!(key_for('?'), ('/', true));
        assert_eq!(key_for('"'), ('\'', true));
        assert_eq!(key_for('|'), ('\\', true));
        assert_eq!(key_for(' '), (' ', false));
    }

    #[test]
    fn typing_presses_what_changed() {
        let mut art = Art::new();
        art.observe("draft");
        assert!(art.presses.is_empty(), "the first text seen is not a keystroke");
        art.observe("draftS");
        let keys: Vec<char> = art.presses.iter().map(|(k, _)| *k).collect();
        assert!(keys.contains(&'s') && keys.contains(&SHIFT));
        art.observe("draft");
        assert!(art.presses.iter().any(|(k, _)| *k == DELETE));
    }

    #[test]
    fn every_main_block_key_types_one_character() {
        let keys: Vec<char> = MODEL.groups.iter().flat_map(|g| &g.solids).filter_map(|s| s.key).collect();
        for c in "`1234567890-=qwertyuiop[]\\asdfghjkl;',./zxcvbnm \n\t".chars() {
            assert_eq!(keys.iter().filter(|&&k| k == c).count(), 1, "{c:?}");
        }
    }

    #[test]
    fn typed_characters_drop_down_the_columns_in_turn() {
        let tip = |art: &Art, c: usize| art.tips[c].as_ref().map(|t| t.to_string());
        let mut art = Art::new();
        art.observe("draft");
        assert!(art.tips.iter().all(Option::is_none), "a restored draft drops nothing");
        art.observe("draft a");
        assert_eq!((tip(&art, 0), tip(&art, 1)), (None, Some("a".into())), "a blank takes its turn");
        assert_eq!(art.rain[1].head, 0.);
        art.observe("draft");
        assert_eq!(art.column, 2, "deleting drops nothing");
        // Past the last column it wraps round.
        let text: String = std::iter::repeat('x').take(COLS - 1).collect();
        art.observe(&format!("draft{text}"));
        assert_eq!(art.column, 1);
        assert_eq!(tip(&art, 0), Some("x".into()));
        // The tip goes with its drop once that leaves the glass.
        art.rain[0].head = ROWS as f32 + art.rain[0].len + 1.;
        art.advance();
        assert_eq!(art.tips[0], None);
    }
}
