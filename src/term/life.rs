//! Conway's Game of Life spinner - a 1-row toroidal GoL that actually does
//! something (patterns glide, blinkers flash) instead of just spinning.
//!
//! Runs in a background thread; the main loop stops it when the first token
//! arrives.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use super::{CLEAR_LINE, DIM, HIDE_CURSOR, RESET, SHOW_CURSOR};

const GOL_W: usize = 24;
const GOL_ALIVE: &str = "█";
const GOL_DEAD: &str = "·";

/// Glider pattern: 5 cells, period-4.
const GLIDER: [(usize, usize); 5] = [(0, 0), (1, 1), (2, 1), (0, 2), (1, 0)];

/// A Conway's Game of Life spinner. Start it before a long operation; stop
/// it when the operation completes.
pub struct LifeSpinner {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl LifeSpinner {
    /// Create a new spinner with the given label (shown before the cells).
    pub fn new(label: &str) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let label = label.to_string();

        let handle = if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
            Some(thread::spawn(move || {
                run(&label, &stop_clone);
            }))
        } else {
            None
        };

        LifeSpinner { stop, handle }
    }

    /// Start the spinner (alias for `new` with "thinking" label).
    pub fn start(label: &str) -> Self {
        Self::new(label)
    }

    /// Stop the spinner and clean up the terminal line.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
            print!("{}{}", CLEAR_LINE, SHOW_CURSOR);
            let _ = std::io::stdout().flush();
        }
    }
}

impl Drop for LifeSpinner {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(label: &str, stop: &AtomicBool) {
    use std::io::Write;
    let mut stdout = std::io::stdout();
    let _ = write!(stdout, "{}", HIDE_CURSOR);

    let mut row = seed();
    let mut frame = 0;

    // Initial render.
    render(&mut stdout, label, &row);
    let _ = stdout.flush();
    super::set_working_title(frame);

    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(90));
        if stop.load(Ordering::Relaxed) {
            break;
        }
        frame += 1;
        super::set_working_title(frame);
        row = step(&row);
        render(&mut stdout, label, &row);
        let _ = stdout.flush();
    }
}

fn render(stdout: &mut impl Write, label: &str, row: &[u8]) {
    let cells: String = row
        .iter()
        .map(|&c| if c != 0 { GOL_ALIVE } else { GOL_DEAD })
        .collect();
    let _ = write!(
        stdout,
        "{}  {}{} {}{}",
        CLEAR_LINE, DIM, label, RESET, cells
    );
}

fn seed() -> Vec<u8> {
    let mut row = vec![0u8; GOL_W];
    let mut rng = SimpleRng::new();

    // Place a glider.
    let x = rng.next() as usize % GOL_W;
    for &(dx, _) in &GLIDER {
        row[(x + dx) % GOL_W] = 1;
    }

    // Two blinkers.
    for _ in 0..2 {
        let x = rng.next() as usize % GOL_W;
        row[x % GOL_W] = 1;
        row[(x + 1) % GOL_W] = 1;
        row[(x + 2) % GOL_W] = 1;
    }

    // Random noise.
    for _ in 0..(GOL_W / 6) {
        row[rng.next() as usize % GOL_W] = 1;
    }

    row
}

fn step(row: &[u8]) -> Vec<u8> {
    let w = row.len();
    let mut nxt = vec![0u8; w];
    for x in 0..w {
        // A 1-row GoL is degenerate (cells have only 2 neighbors). Cheat:
        // treat the row as the middle of a 3-row toroidal world where the
        // rows above and below mirror the current one. Gives every cell the
        // standard 8 neighbors, so gliders/blinkers work.
        let n = row[(x + w - 1) % w] as usize
            + row[x] as usize
            + row[(x + 1) % w] as usize
            + row[(x + w - 1) % w] as usize
            + row[(x + 1) % w] as usize
            + row[(x + w - 1) % w] as usize
            + row[x] as usize
            + row[(x + 1) % w] as usize;
        let cur = row[x] != 0;
        nxt[x] = if (cur && (n == 2 || n == 3)) || (!cur && n == 3) {
            1
        } else {
            0
        };
    }
    nxt
}

/// Tiny xorshift RNG for seeding the spinner pattern.
struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0xdeadbeef);
        Self { state: seed }
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
