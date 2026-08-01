use std::{
    env,
    io::{self, IsTerminal, Write},
    time::{Duration, Instant},
};

pub struct BuildProgress {
    project: String,
    total: usize,
    current: usize,
    started: Instant,
    interactive: bool,
    colour: bool,
    finished: bool,
}

const BAR_WIDTH: usize = 24;
const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[38;2;166;227;161m";
const MAUVE: &str = "\x1b[38;2;203;166;247m";
const ERROR: &str = "\x1b[38;2;243;139;168m";

const GRADIENT: [(u8, u8, u8); 4] =
    [(243, 139, 168), (250, 179, 135), (249, 226, 175), (148, 226, 213)];

impl BuildProgress {
    pub fn new(project: &str, total: usize) -> Self {
        assert!(total > 0, "a build progress bar must contain at least one phase");

        let interactive =
            io::stderr().is_terminal() && env::var_os("TERM").as_deref() != Some("dumb".as_ref());

        Self {
            project: sanitise(project),
            total,
            current: 0,
            started: Instant::now(),
            interactive,
            colour: interactive && env::var_os("NO_COLOR").is_none(),
            finished: false,
        }
    }

    pub fn phase(&mut self, label: &str) {
        assert!(self.current < self.total, "build progress advanced beyond its final phase");
        self.render("◆", label, self.current);
        self.current += 1;
    }

    pub fn set_total(&mut self, total: usize) {
        assert!(total >= self.current, "build progress cannot discard completed phases");
        self.total = total;
    }

    pub fn finish(mut self) {
        self.render("✓", "Finished", self.total);
        self.finished = true;
    }

    fn render(&self, icon: &str, label: &str, completed: usize) {
        let line = self.line(icon, label, completed);
        let mut stderr = io::stderr().lock();

        match self.interactive {
            true => {
                let _ = write!(stderr, "\r\x1b[2K{line}");
                if completed == self.total {
                    let _ = writeln!(stderr);
                }
            },
            false => {
                let _ = writeln!(stderr, "{line}");
            },
        }

        let _ = stderr.flush();
    }

    fn line(&self, icon: &str, label: &str, completed: usize) -> String {
        let percentage = completed * 100 / self.total;
        let elapsed = format_elapsed(self.started.elapsed());
        let bar = render_bar(completed, self.total, self.colour);

        match self.colour {
            true => {
                let icon_colour = match completed == self.total {
                    true => GREEN,
                    false => MAUVE,
                };
                format!(
                    "  {icon_colour}{icon}{RESET} {label:<18} {bar} {percentage:>3}%  {DIM}{elapsed}{RESET}  {}",
                    self.project,
                )
            },
            false => {
                format!("  {icon} {label:<18} {bar} {percentage:>3}%  {elapsed}  {}", self.project,)
            },
        }
    }
}

impl Drop for BuildProgress {
    fn drop(&mut self) {
        if self.finished {
            return;
        }

        let line = match self.colour {
            true => format!(
                "  {ERROR}✗{RESET} {:<18} {}  {}",
                "Failed",
                format_elapsed(self.started.elapsed()),
                self.project,
            ),
            false => format!(
                "  ✗ {:<18} {}  {}",
                "Failed",
                format_elapsed(self.started.elapsed()),
                self.project,
            ),
        };
        let mut stderr = io::stderr().lock();

        match self.interactive {
            true => {
                let _ = writeln!(stderr, "\r\x1b[2K{line}");
            },
            false => {
                let _ = writeln!(stderr, "{line}");
            },
        }
    }
}

fn render_bar(completed: usize, total: usize, colour: bool) -> String {
    let filled = completed * BAR_WIDTH / total;
    let mut bar = String::with_capacity(BAR_WIDTH * 20);

    bar.push('[');
    for index in 0..BAR_WIDTH {
        match index < filled {
            true if colour => {
                let (red, green, blue) = gradient_colour(index);
                bar.push_str(&format!("\x1b[38;2;{red};{green};{blue}m━"));
            },
            true => bar.push('━'),
            false if colour => bar.push_str("\x1b[2;37m─"),
            false => bar.push('─'),
        }
    }
    if colour {
        bar.push_str(RESET);
    }
    bar.push(']');

    bar
}

fn gradient_colour(index: usize) -> (u8, u8, u8) {
    let scaled = index * (GRADIENT.len() - 1);
    let segment = (scaled / (BAR_WIDTH - 1)).min(GRADIENT.len() - 2);
    let segment_start = segment * (BAR_WIDTH - 1);
    let offset = scaled - segment_start;
    let from = GRADIENT[segment];
    let to = GRADIENT[segment + 1];

    (
        interpolate(from.0, to.0, offset),
        interpolate(from.1, to.1, offset),
        interpolate(from.2, to.2, offset),
    )
}

fn interpolate(from: u8, to: u8, offset: usize) -> u8 {
    let distance = BAR_WIDTH - 1;
    let from = i32::from(from);
    let delta = i32::from(to) - from;

    (from + delta * offset as i32 / distance as i32) as u8
}

fn format_elapsed(elapsed: Duration) -> String {
    match elapsed.as_secs() {
        0 => format!("{:.2}s", elapsed.as_secs_f64()),
        seconds => format!("{seconds}.{tenths}s", tenths = elapsed.subsec_millis() / 100),
    }
}

fn sanitise(value: &str) -> String {
    value.chars().filter(|character| !character.is_control()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_bar_tracks_completed_phases() {
        assert_eq!(render_bar(0, 4, false), "[────────────────────────]");
        assert_eq!(render_bar(2, 4, false), "[━━━━━━━━━━━━────────────]");
        assert_eq!(render_bar(4, 4, false), "[━━━━━━━━━━━━━━━━━━━━━━━━]");
    }

    #[test]
    fn elapsed_time_uses_compact_precision() {
        assert_eq!(format_elapsed(Duration::from_millis(42)), "0.04s");
        assert_eq!(format_elapsed(Duration::from_millis(1_234)), "1.2s");
    }

    #[test]
    fn terminal_controls_are_removed_from_project_names() {
        assert_eq!(sanitise("nyx\n\x1b[31m"), "nyx[31m");
    }
}
