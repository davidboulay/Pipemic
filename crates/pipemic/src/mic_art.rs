//! Vector drawing of the Wave:3 with its front LEDs.
//!
//! Layout follows the real mic: a rounded capsule, mesh grille on the upper
//! part, a plain lower panel with three mode marks, a row of LEDs and the
//! knob; side thumbscrews on a U yoke, a pole and a round base.
//!
//! The mic doesn't report its LEDs, so they mirror what the daemon knows:
//! the knob's mode picks the mark and what the row shows. Gain and
//! headphone volume fill from the left; the monitor mix lights a single
//! LED at the balance point. Muted turns the row red, as on the device.

use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::{Color, Point, Rectangle, Renderer, Size, Theme};

const BODY: Color = Color::from_rgb(0.13, 0.135, 0.15);
const PANEL: Color = Color::from_rgb(0.105, 0.11, 0.12);
const EDGE: Color = Color::from_rgb(0.27, 0.28, 0.31);
const MESH: Color = Color::from_rgb(0.25, 0.26, 0.29);
const LED_OFF: Color = Color::from_rgb(0.22, 0.23, 0.26);
const LED_ON: Color = Color::from_rgb(0.95, 0.96, 0.98);
const LED_MUTE: Color = Color::from_rgb(1.0, 0.27, 0.23);
const METAL: Color = Color::from_rgb(0.09, 0.09, 0.10);
const METAL_EDGE: Color = Color::from_rgb(0.20, 0.20, 0.22);

/// LEDs in the front row.
const LEDS: usize = 9;

#[derive(Clone, Copy, PartialEq)]
pub enum Knob {
    Gain,
    Headphone,
    Mix,
}

pub struct MicArt {
    /// 0..=1 position of whatever the knob controls.
    pub level: f32,
    pub knob: Knob,
    pub muted: bool,
}

/// Is (x, y) inside a rounded rectangle?
fn inside(x: f32, y: f32, l: f32, t: f32, w: f32, h: f32, r: f32) -> bool {
    if x < l || x > l + w || y < t || y > t + h {
        return false;
    }
    let cx = x.clamp(l + r, l + w - r);
    let cy = y.clamp(t + r, t + h - r);
    (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

impl<Message> canvas::Program<Message> for MicArt {
    type State = ();

    fn draw(&self, _: &(), renderer: &Renderer, _: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        let mut f = Frame::new(renderer, bounds.size());
        // Design space 180 × 350, scaled to fit and centred. Proportions from
        // the product photo: body 84 × 225 (≈ 1 : 2.7), grille on the top
        // 56 %, screws at the grille/panel seam, base ≈ 1.7 × body width.
        let (dw, dh) = (180.0, 350.0);
        let s = (bounds.width / dw).min(bounds.height / dh);
        let ox = (bounds.width - dw * s) / 2.0;
        let oy = (bounds.height - dh * s) / 2.0;
        let p = |x: f32, y: f32| Point::new(ox + x * s, oy + y * s);
        let sz = |w: f32, h: f32| Size::new(w * s, h * s);
        let rr = |x: f32, y: f32, w: f32, h: f32, r: f32| Path::rounded_rectangle(p(x, y), sz(w, h), (r * s).into());
        let stroke = |c: Color, w: f32| Stroke::default().with_color(c).with_width(w * s);

        let (bl, bt, bw, bh, br) = (48.0, 18.0, 84.0, 225.0, 34.0);
        let cx = bl + bw / 2.0;
        let seam = bt + bh * 0.56;

        // Base: a flat disc seen slightly from above.
        let disc = |ry: f32, y: f32| {
            Path::new(|b| {
                b.ellipse(canvas::path::arc::Elliptical {
                    center: p(cx, y),
                    radii: iced::Vector::new(71.0 * s, ry * s),
                    rotation: iced::Radians(0.0),
                    start_angle: iced::Radians(0.0),
                    end_angle: iced::Radians(std::f32::consts::TAU),
                })
            })
        };
        f.fill(&disc(10.0, 334.0), METAL);
        f.fill(&disc(9.0, 331.0), Color::from_rgb(0.12, 0.12, 0.13));
        // Pole, from the yoke down into the base.
        f.fill(&rr(cx - 4.0, 268.0, 8.0, 64.0, 2.0), METAL);

        // U yoke hugging the body: arms from the screws, round under the body.
        let (yl, yr, yb) = (bl - 8.0, bl + bw + 8.0, 270.0);
        let yoke = Path::new(|b| {
            b.move_to(p(yl, seam));
            b.line_to(p(yl, yb - 40.0));
            b.arc_to(p(yl, yb), p(cx, yb), 40.0 * s);
            b.arc_to(p(yr, yb), p(yr, yb - 40.0), 40.0 * s);
            b.line_to(p(yr, seam));
        });
        f.stroke(&yoke, stroke(METAL, 6.0));
        // Thumbscrews (knurled caps) at the seam.
        for x in [yl - 9.0, yr - 1.0] {
            f.fill(&rr(x, seam - 9.0, 10.0, 18.0, 3.0), METAL);
            for i in 0..4 {
                let yy = seam - 6.0 + i as f32 * 4.0;
                f.stroke(&Path::line(p(x + 2.0, yy), p(x + 8.0, yy)), stroke(METAL_EDGE, 0.9));
            }
        }

        // Body capsule.
        let body = rr(bl, bt, bw, bh, br);
        f.fill(&body, BODY);
        // Grille: a fine dot mesh down to the seam.
        let mut y = bt + 4.0;
        let mut row = 0;
        while y < seam - 1.0 {
            let mut x = bl + 4.0 + if row % 2 == 0 { 0.0 } else { 1.8 };
            while x < bl + bw - 3.0 {
                if inside(x, y, bl + 2.5, bt + 2.5, bw - 5.0, bh - 5.0, br - 2.5) {
                    f.fill(&Path::circle(p(x, y), 0.85 * s), MESH);
                }
                x += 3.6;
            }
            y += 3.2;
            row += 1;
        }
        // Lower panel below the seam.
        let panel = Path::new(|b| {
            b.move_to(p(bl + 2.5, seam));
            b.line_to(p(bl + bw - 2.5, seam));
            b.line_to(p(bl + bw - 2.5, bt + bh - br));
            b.arc_to(p(bl + bw - 2.5, bt + bh - 2.5), p(cx, bt + bh - 2.5), (br - 2.5) * s);
            b.arc_to(p(bl + 2.5, bt + bh - 2.5), p(bl + 2.5, bt + bh - br), (br - 2.5) * s);
            b.close();
        });
        f.fill(&panel, PANEL);
        f.stroke(&body, stroke(EDGE, 1.2));
        // Badge spot near the top of the grille (plain ring, no logo).
        f.fill(&Path::circle(p(cx, bt + 26.0), 7.5 * s), BODY);
        f.stroke(&Path::circle(p(cx, bt + 26.0), 7.5 * s), stroke(EDGE, 1.2));
        // Touch pad on top: glows red while muted.
        f.stroke(
            &Path::line(p(cx - 18.0, bt + 1.5), p(cx + 18.0, bt + 1.5)),
            stroke(if self.muted { LED_MUTE } else { EDGE }, 2.5).with_line_cap(canvas::LineCap::Round),
        );

        // Mode marks: gain, headphones, mix; the active one is lit.
        let marks_y = bt + bh * 0.67;
        for (k, dx) in [(Knob::Gain, -11.0), (Knob::Headphone, 0.0), (Knob::Mix, 11.0)] {
            let c = if k == self.knob { LED_ON } else { LED_OFF };
            f.fill(&Path::circle(p(cx + dx, marks_y), 1.7 * s), c);
        }

        // LED row.
        let led_y = bt + bh * 0.75;
        let lvl = self.level.clamp(0.0, 1.0);
        // On the device the mix LED runs the other way: all-mic sits at the left.
        let single = (LEDS - 1) - ((lvl * (LEDS - 1) as f32).round()) as usize;
        let fill = (lvl * LEDS as f32).round() as usize;
        let step = 4.6;
        let x0 = cx - step * (LEDS - 1) as f32 / 2.0;
        for i in 0..LEDS {
            let lit = match self.knob {
                _ if self.muted => true,
                Knob::Mix => i == single,
                _ => i < fill,
            };
            let c = match (lit, self.muted) {
                (false, _) => LED_OFF,
                (true, true) => LED_MUTE,
                (true, false) => LED_ON,
            };
            f.fill(&Path::circle(p(x0 + i as f32 * step, led_y), 1.5 * s), c);
        }

        // Knob: about a quarter of the body width.
        let k = p(cx, bt + bh * 0.83);
        f.fill(&Path::circle(k, 10.5 * s), METAL);
        f.stroke(&Path::circle(k, 10.5 * s), stroke(METAL_EDGE, 1.5));
        f.stroke(&Path::circle(k, 7.5 * s), stroke(Color { a: 0.5, ..METAL_EDGE }, 0.8));

        vec![f.into_geometry()]
    }
}
