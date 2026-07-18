//! Conway's Game of Life spinner - a 1-row toroidal `GoL` that actually does
//! something (patterns glide, blinkers flash) instead of just spinning.
//!
//! The simulation is pure (`seed`/`step`) and the spinner state renders through
//! Ratatui as a single styled line. The `activity` event loop owns a
//! `LifeSpinner`, advancing it one generation per animation tick while a model
//! request is in flight; there is no background thread writing raw escapes.

use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

const GOL_W: usize = 24;
const GOL_ALIVE: char = '\u{2588}'; // full block
const GOL_DEAD: char = '\u{b7}'; // middle dot

/// Glider pattern: 5 cells, period-4.
const GLIDER: [(usize, usize); 5] = [(0, 0), (1, 1), (2, 1), (0, 2), (1, 0)];

/// Animated Game-of-Life spinner state. Advance it with [`LifeSpinner::tick`]
/// and draw it with [`LifeSpinner::render`].
pub struct LifeSpinner {
    row: Vec<u8>,
    label: String,
    frame: usize,
}

impl LifeSpinner {
    /// Create a spinner seeded from the wall clock, labelled `label`.
    #[must_use]
    pub fn new(label: &str) -> Self {
        Self {
            row: seed(&mut SimpleRng::from_clock()),
            label: label.to_string(),
            frame: 0,
        }
    }

    /// Advance the simulation one generation and bump the frame counter.
    pub fn tick(&mut self) {
        self.row = step(&self.row);
        self.frame = self.frame.wrapping_add(1);
    }

    /// The current frame index (drives the OSC title glyph).
    #[must_use]
    pub fn frame(&self) -> usize {
        self.frame
    }

    /// The current row rendered as a styled Ratatui line: dim label, then the
    /// live/dead cells.
    #[must_use]
    pub fn line(&self) -> Line<'static> {
        let cells: String = self
            .row
            .iter()
            .map(|&c| if c != 0 { GOL_ALIVE } else { GOL_DEAD })
            .collect();
        Line::from(vec![
            Span::from(format!("  {} ", self.label)).dim(),
            Span::from(cells),
        ])
    }

    /// Draw the spinner into `area`.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(self.line(), area);
    }
}

/// Seed a fresh row: one glider, two blinkers, and a little noise.
fn seed(rng: &mut SimpleRng) -> Vec<u8> {
    let mut row = vec![0u8; GOL_W];

    let x = rng.next() as usize % GOL_W;
    for &(dx, _) in &GLIDER {
        row[(x + dx) % GOL_W] = 1;
    }

    for _ in 0..2 {
        let x = rng.next() as usize % GOL_W;
        row[x % GOL_W] = 1;
        row[(x + 1) % GOL_W] = 1;
        row[(x + 2) % GOL_W] = 1;
    }

    for _ in 0..(GOL_W / 6) {
        row[rng.next() as usize % GOL_W] = 1;
    }

    row
}

/// Advance one generation. A 1-row `GoL` is degenerate (cells have only two
/// neighbours), so we treat the row as the middle of a 3-row toroidal world
/// whose top and bottom rows mirror it, giving every cell the standard eight
/// neighbours so gliders and blinkers behave.
fn step(row: &[u8]) -> Vec<u8> {
    let w = row.len();
    let mut nxt = vec![0u8; w];
    for x in 0..w {
        let left = row[(x + w - 1) % w] as usize;
        let right = row[(x + 1) % w] as usize;
        let center = row[x] as usize;
        // left/right counted in all three mirrored rows; center in top+bottom.
        let n = left * 3 + right * 3 + center * 2;
        let alive = row[x] != 0;
        nxt[x] = u8::from(n == 3 || (n == 2 && alive));
    }
    nxt
}

/// Tiny xorshift RNG for seeding the spinner pattern.
struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    fn from_clock() -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0xdead_beef, |d| d.as_secs() ^ u64::from(d.subsec_nanos()));
        Self::from_seed(seed)
    }

    fn from_seed(seed: u64) -> Self {
        Self {
            state: seed | 1, // avoid the all-zero fixed point
        }
    }

    fn next(&mut self) -> u8 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        (x & 0xff) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_is_deterministic() {
        let mut rng = SimpleRng::from_seed(42);
        let row = seed(&mut rng);
        assert_eq!(step(&row), step(&row));
    }

    #[test]
    fn all_dead_stays_dead() {
        let row = vec![0u8; GOL_W];
        assert_eq!(step(&row), row);
    }

    #[test]
    fn seed_is_reproducible_for_a_fixed_seed() {
        let a = seed(&mut SimpleRng::from_seed(7));
        let b = seed(&mut SimpleRng::from_seed(7));
        assert_eq!(a, b);
        assert_eq!(a.len(), GOL_W);
        assert!(a.iter().any(|&c| c != 0), "seed should place live cells");
    }

    #[test]
    fn line_has_label_and_full_width_cells() {
        let sp = LifeSpinner {
            row: vec![0u8; GOL_W],
            label: "thinking".to_string(),
            frame: 0,
        };
        let rendered = sp.line().to_string();
        assert!(rendered.contains("thinking"));
        let cells = rendered.matches(GOL_DEAD).count();
        assert_eq!(cells, GOL_W);
    }

    #[test]
    fn tick_advances_frame() {
        let mut sp = LifeSpinner::new("x");
        let f0 = sp.frame();
        sp.tick();
        assert_eq!(sp.frame(), f0 + 1);
    }

    #[test]
    fn renders_label_into_a_test_backend_buffer() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let sp = LifeSpinner {
            row: vec![0u8; GOL_W],
            label: "thinking".to_string(),
            frame: 0,
        };
        let mut terminal = Terminal::new(TestBackend::new(40, 1)).unwrap();
        terminal.draw(|f| sp.render(f, f.area())).unwrap();
        let buf = terminal.backend().buffer().clone();
        let row: String = (0..40).map(|x| buf[(x, 0)].symbol().to_owned()).collect();
        assert!(row.contains("thinking"), "spinner row: {row:?}");
    }
}
