//! petri — a Petri net player for the terminal.
//!
//! Load a net from a plain text file, watch the marking, fire transitions
//! by hand or let a random run resolve the conflicts, and analyze the
//! reachable state space for deadlocks and place bounds. Conflicts (two
//! enabled transitions competing for the same tokens) are marked: they are
//! the points the formalism leaves open.

use crust::style;
use crust::{Crust, Input, Pane, Popup};
use std::collections::{HashSet, VecDeque};

// Colours (256-palette).
const C_TOKEN: u16 = 220; // token dots
const C_PLACE: u16 = 250;
const C_ENABLED: u16 = 46;
const C_DISABLED: u16 = 243;
const C_CONFLICT: u16 = 208;
const C_HEADER_BG: u16 = 17;
const C_LOG: u16 = 245;

struct PlaceDef {
    name: String,
    init: u32,
}

struct Trans {
    name: String,
    ins: Vec<usize>,  // place indices, repetition = arc weight
    outs: Vec<usize>,
}

struct Net {
    places: Vec<PlaceDef>,
    trans: Vec<Trans>,
}

impl Net {
    /// Parse the net format. Only two line shapes matter:
    ///   `name = N`                  initial marking for a place
    ///   `[name:] in in -> out out`  a transition (repeat a place = weight)
    /// Everything else (headings, indentation, comments after #) is ignored,
    /// so a HyperList-shaped file parses as-is.
    fn parse(text: &str) -> Result<Net, String> {
        fn place_idx(places: &mut Vec<PlaceDef>, name: &str) -> usize {
            if let Some(i) = places.iter().position(|p| p.name == name) {
                return i;
            }
            places.push(PlaceDef { name: name.to_string(), init: 0 });
            places.len() - 1
        }

        let mut places: Vec<PlaceDef> = Vec::new();
        let mut trans: Vec<Trans> = Vec::new();

        for (lno, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some((lhs, rhs)) = line.split_once("->") {
                let (name, ins_str) = match lhs.split_once(':') {
                    Some((n, i)) => (n.trim().to_string(), i),
                    None => (format!("t{}", trans.len() + 1), lhs),
                };
                let name = if name.is_empty() { format!("t{}", trans.len() + 1) } else { name };
                let ins: Vec<usize> = ins_str
                    .split_whitespace()
                    .map(|w| place_idx(&mut places, w))
                    .collect();
                let outs: Vec<usize> = rhs
                    .split_whitespace()
                    .map(|w| place_idx(&mut places, w))
                    .collect();
                if ins.is_empty() && outs.is_empty() {
                    return Err(format!("line {}: transition with no places", lno + 1));
                }
                trans.push(Trans { name, ins, outs });
            } else if let Some((n, v)) = line.split_once('=') {
                let n = n.trim();
                if n.split_whitespace().count() != 1 {
                    continue; // not a place line, ignore as structure
                }
                let tokens: u32 = v
                    .trim()
                    .parse()
                    .map_err(|_| format!("line {}: bad token count '{}'", lno + 1, v.trim()))?;
                let idx = place_idx(&mut places, n);
                places[idx].init = tokens;
            }
            // anything else: heading / prose — ignored
        }
        if trans.is_empty() {
            return Err("no transitions found (need lines like  name: a b -> c)".into());
        }
        Ok(Net { places, trans })
    }

    fn initial_marking(&self) -> Vec<u32> {
        self.places.iter().map(|p| p.init).collect()
    }

    /// Is transition `t` enabled under marking `m`? (weight-aware)
    fn enabled(&self, m: &[u32], t: usize) -> bool {
        let mut tmp = m.to_vec();
        for &p in &self.trans[t].ins {
            if tmp[p] == 0 {
                return false;
            }
            tmp[p] -= 1;
        }
        true
    }

    fn enabled_set(&self, m: &[u32]) -> Vec<usize> {
        (0..self.trans.len()).filter(|&t| self.enabled(m, t)).collect()
    }

    /// Fire transition `t` (caller ensures it is enabled).
    fn fire(&self, m: &[u32], t: usize) -> Vec<u32> {
        let mut m2 = m.to_vec();
        for &p in &self.trans[t].ins {
            m2[p] -= 1;
        }
        for &p in &self.trans[t].outs {
            m2[p] += 1;
        }
        m2
    }

    /// Transitions in conflict with `t` under `m`: enabled now, but disabled
    /// once `t` fires. The choice between them is exactly what the net
    /// cannot resolve.
    fn conflicts(&self, m: &[u32], t: usize, enabled: &[usize]) -> bool {
        if !enabled.contains(&t) {
            return false;
        }
        let after = {
            let mut tmp = m.to_vec();
            for &p in &self.trans[t].ins {
                tmp[p] -= 1;
            }
            tmp
        };
        enabled.iter().any(|&o| o != t && !self.enabled(&after, o))
    }

    fn arcs_str(&self, t: usize) -> String {
        let side = |v: &[usize]| {
            if v.is_empty() {
                "\u{2205}".to_string() // ∅
            } else {
                v.iter().map(|&p| self.places[p].name.as_str()).collect::<Vec<_>>().join(" ")
            }
        };
        format!("{} \u{2192} {}", side(&self.trans[t].ins), side(&self.trans[t].outs))
    }
}

struct Analysis {
    states: usize,
    truncated: bool,
    deadlocks: Vec<Vec<u32>>, // up to 3 examples
    deadlock_count: usize,
    max_tokens: Vec<u32>,
}

/// Bounded breadth-first exploration of the reachable markings.
fn analyze(net: &Net, m0: &[u32], cap: usize) -> Analysis {
    let mut seen: HashSet<Vec<u32>> = HashSet::new();
    let mut queue: VecDeque<Vec<u32>> = VecDeque::new();
    let mut a = Analysis {
        states: 0,
        truncated: false,
        deadlocks: Vec::new(),
        deadlock_count: 0,
        max_tokens: m0.to_vec(),
    };
    seen.insert(m0.to_vec());
    queue.push_back(m0.to_vec());
    while let Some(m) = queue.pop_front() {
        a.states += 1;
        for (i, &v) in m.iter().enumerate() {
            if v > a.max_tokens[i] {
                a.max_tokens[i] = v;
            }
        }
        let en = net.enabled_set(&m);
        if en.is_empty() {
            a.deadlock_count += 1;
            if a.deadlocks.len() < 3 {
                a.deadlocks.push(m.clone());
            }
            continue;
        }
        for t in en {
            let m2 = net.fire(&m, t);
            if seen.len() >= cap {
                a.truncated = true;
                continue;
            }
            if seen.insert(m2.clone()) {
                queue.push_back(m2);
            }
        }
        if a.truncated && queue.is_empty() {
            break;
        }
    }
    a
}

fn marking_str(net: &Net, m: &[u32]) -> String {
    let parts: Vec<String> = net
        .places
        .iter()
        .enumerate()
        .filter(|(i, _)| m[*i] > 0)
        .map(|(i, p)| {
            if m[i] == 1 {
                p.name.clone()
            } else {
                format!("{}\u{00d7}{}", p.name, m[i])
            }
        })
        .collect();
    if parts.is_empty() {
        "(empty)".to_string()
    } else {
        parts.join(" ")
    }
}

fn analysis_text(net: &Net, a: &Analysis) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        " Reachable markings: {}{}\n",
        a.states,
        if a.truncated { "+ (truncated — possibly unbounded)" } else { "" }
    ));
    let maxb = *a.max_tokens.iter().max().unwrap_or(&0);
    if !a.truncated {
        s.push_str(&format!(
            " Bounded: yes ({}-bounded{})\n",
            maxb,
            if maxb <= 1 { " — safe" } else { "" }
        ));
    } else {
        s.push_str(" Bounded: unknown (exploration capped)\n");
    }
    if a.deadlock_count == 0 {
        s.push_str(&format!(
            " Deadlocks: none{}\n",
            if a.truncated { " found (within cap)" } else { " — deadlock-free" }
        ));
    } else {
        s.push_str(&format!(" Deadlocks: {} dead marking(s) reachable!\n", a.deadlock_count));
        for m in &a.deadlocks {
            s.push_str(&format!("   dead: {}\n", marking_str(net, m)));
        }
    }
    s.push_str("\n Place bounds (max tokens seen):\n");
    for (i, p) in net.places.iter().enumerate() {
        s.push_str(&format!("   {:<16} {}\n", p.name, a.max_tokens[i]));
    }
    s.push_str("\n (Esc/q to close)");
    s
}

/// Tiny xorshift PRNG seeded from the clock — only used to resolve
/// conflicts during auto-run. The net itself stays deterministic.
struct Rng(u64);
impl Rng {
    fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64 | 1)
            .unwrap_or(0x9e3779b9);
        Rng(seed | 1)
    }
    fn next(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Focus {
    Places,
    Trans,
}

struct App {
    net: Net,
    marking: Vec<u32>,
    undo: Vec<Vec<u32>>,
    focus: Focus,
    pidx: usize,
    tidx: usize,
    running: bool,
    steps: u64,
    log: VecDeque<String>,
    file: Option<String>,
    rng: Rng,
    header: Pane,
    places: Pane,
    trans: Pane,
    logp: Pane,
    status: Pane,
    msg: Option<String>,
}

const C_BORDER_FOCUS: u16 = 75;
const C_BORDER_DIM: u16 = 238;
const C_SEL_BG: u16 = 24;

impl App {
    fn new(net: Net, file: Option<String>) -> Self {
        let marking = net.initial_marking();
        let mut app = App {
            net,
            marking,
            undo: Vec::new(),
            focus: Focus::Trans,
            pidx: 0,
            tidx: 0,
            running: false,
            steps: 0,
            log: VecDeque::new(),
            file,
            rng: Rng::new(),
            header: Pane::new(1, 1, 80, 1, 231, C_HEADER_BG),
            places: Pane::new(1, 2, 40, 20, 250, 234),
            trans: Pane::new(41, 2, 40, 20, 250, 233),
            logp: Pane::new(1, 22, 80, 5, C_LOG, 232),
            status: Pane::new(1, 27, 80, 1, 250, 236),
            msg: None,
        };
        app.layout();
        app
    }

    /// Compact, bordered layout anchored top-left: panes sized to the net,
    /// the trace filling the leftover height. On a big terminal the content
    /// stays together instead of scattering into the corners.
    fn layout(&mut self) {
        let (cols, rows) = Crust::terminal_size();
        let rows = rows.max(14);
        let cols = cols.max(60);

        // widths sized to content
        let max_pname = self.net.places.iter().map(|p| p.name.len()).max().unwrap_or(6);
        let max_tname = self.net.trans.iter().map(|t| t.name.len()).max().unwrap_or(6).max(8);
        let max_arcs = (0..self.net.trans.len())
            .map(|t| self.net.arcs_str(t).chars().count())
            .max()
            .unwrap_or(10);
        let wp = ((max_pname + 20) as u16).clamp(24, cols / 2 - 4);
        let xt = 2 + wp + 3;
        let wt = ((max_tname + max_arcs + 8) as u16).min(cols - xt - 1).max(20);

        // heights: panes hug the item count, trace takes the rest
        let items = self.net.places.len().max(self.net.trans.len()) as u16;
        let main_h = (items + 1).clamp(4, rows.saturating_sub(12).max(4));
        let log_y = 3 + main_h + 2;
        let log_h = rows.saturating_sub(log_y + 2).max(2);

        self.header = Pane::new(1, 1, cols, 1, 231, C_HEADER_BG);
        self.places = Pane::new(2, 3, wp, main_h, 250, 233);
        self.trans = Pane::new(xt, 3, wt, main_h, 250, 233);
        self.logp = Pane::new(2, log_y, cols - 2, log_h, C_LOG, 233);
        self.status = Pane::new(1, rows, cols, 1, 250, 236);
        for p in [&mut self.header, &mut self.places, &mut self.trans, &mut self.logp, &mut self.status] {
            p.scroll = false;
            p.wrap = false;
        }
        for p in [&mut self.places, &mut self.trans, &mut self.logp] {
            p.border = true;
            p.border_fg = Some(C_BORDER_DIM);
        }
        Crust::clear_screen();
    }

    fn push_log(&mut self, s: String) {
        self.log.push_back(s);
        while self.log.len() > 200 {
            self.log.pop_front();
        }
    }

    fn fire(&mut self, t: usize) {
        if !self.net.enabled(&self.marking, t) {
            self.msg = Some(format!(" {} is not enabled", self.net.trans[t].name));
            return;
        }
        let en = self.net.enabled_set(&self.marking);
        let conflicted = self.net.conflicts(&self.marking, t, &en);
        self.undo.push(self.marking.clone());
        if self.undo.len() > 500 {
            self.undo.remove(0);
        }
        self.marking = self.net.fire(&self.marking, t);
        self.steps += 1;
        self.push_log(format!(
            "{:>4}  {}{}",
            self.steps,
            self.net.trans[t].name,
            if conflicted { "  \u{2039}conflict resolved\u{203a}" } else { "" }
        ));
    }

    fn render(&mut self) {
        let en = self.net.enabled_set(&self.marking);

        // --- header ---
        let fname = self
            .file
            .as_deref()
            .map(|f| f.rsplit('/').next().unwrap_or(f))
            .unwrap_or("(builtin demo)");
        self.header.set_text(&format!(
            " petri  {}  \u{00b7}  {} places  {} transitions  \u{00b7}  step {}  {}",
            fname,
            self.net.places.len(),
            self.net.trans.len(),
            self.steps,
            if self.running { style::bold("\u{25b6} RUNNING") } else { String::new() }
        ));
        self.header.refresh();

        // --- pane borders show focus ---
        self.places.border_fg = Some(if self.focus == Focus::Places { C_BORDER_FOCUS } else { C_BORDER_DIM });
        self.trans.border_fg = Some(if self.focus == Focus::Trans { C_BORDER_FOCUS } else { C_BORDER_DIM });
        self.places.border_refresh();
        self.trans.border_refresh();
        self.logp.border_refresh();

        // --- places ---
        let name_w = self.net.places.iter().map(|p| p.name.len()).max().unwrap_or(6).max(6);
        let mut pl = format!(" {}\n", style::bold("PLACES"));
        for (i, p) in self.net.places.iter().enumerate() {
            let n = self.marking[i];
            let dots = if n == 0 {
                "\u{00b7}".to_string()
            } else if n <= 12 {
                "\u{25cf}".repeat(n as usize)
            } else {
                format!("\u{25cf}\u{00d7}{}", n)
            };
            let sel = self.focus == Focus::Places && i == self.pidx;
            let selpre = if sel {
                format!("{}{}", style::set_bg(C_SEL_BG as u8), style::set_fg(231))
            } else {
                style::set_fg(C_PLACE as u8)
            };
            pl.push_str(&format!(
                " {}{:<nw$}  {}{}\n",
                selpre,
                p.name,
                style::set_fg(if n == 0 { C_DISABLED as u8 } else { C_TOKEN as u8 }),
                format_args!("{}{}", dots, style::RESET),
                nw = name_w
            ));
        }
        self.places.set_text(&pl);
        self.places.ix = (self.pidx + 2).saturating_sub(self.places.h as usize);
        self.places.refresh();

        // --- transitions ---
        let tname_w = self.net.trans.iter().map(|t| t.name.len()).max().unwrap_or(8).max(8);
        let mut tl = format!(" {}\n", style::bold("TRANSITIONS"));
        for (t, tr) in self.net.trans.iter().enumerate() {
            let is_en = en.contains(&t);
            let confl = is_en && self.net.conflicts(&self.marking, t, &en);
            let sel = self.focus == Focus::Trans && t == self.tidx;
            let (mark, col) = if confl {
                ("\u{203c}", C_CONFLICT)
            } else if is_en {
                ("\u{25b6}", C_ENABLED)
            } else {
                ("\u{00b7}", C_DISABLED)
            };
            let selpre = if sel { style::set_bg(C_SEL_BG as u8) } else { String::new() };
            let namecol = if sel { 231 } else if is_en { 250 } else { C_DISABLED };
            tl.push_str(&format!(
                " {}{}{} {}{:<tw$} {}{}{}\n",
                selpre,
                style::set_fg(col as u8),
                mark,
                style::set_fg(namecol as u8),
                tr.name,
                style::set_fg(if is_en { 250 } else { C_DISABLED as u8 }),
                self.net.arcs_str(t),
                tw = tname_w
            ));
        }
        self.trans.set_text(&tl);
        self.trans.ix = (self.tidx + 2).saturating_sub(self.trans.h as usize);
        self.trans.refresh();

        // --- trace ---
        let cap = (self.logp.h as usize).saturating_sub(1).max(1);
        let lines: Vec<&str> = self.log.iter().map(|s| s.as_str()).collect();
        let tail = if lines.len() > cap { &lines[lines.len() - cap..] } else { &lines[..] };
        let mut lg = format!(" {}  (the run so far \u{2014} one path through the branching tree)\n", style::bold("TRACE"));
        lg.push_str(&tail.join("\n"));
        self.logp.set_text(&lg);
        self.logp.refresh();

        // --- status ---
        let status = if let Some(m) = self.msg.take() {
            m
        } else if en.is_empty() {
            format!("{}  u undo \u{00b7} R reset \u{00b7} a analyze", style::styled(" DEADLOCK \u{2014} no enabled transitions ", Some(231), Some(52), ""))
        } else {
            " Tab focus \u{00b7} Enter fire \u{00b7} Spc run \u{00b7} +/- tokens \u{00b7} u undo \u{00b7} R reset \u{00b7} a analyze \u{00b7} e edit \u{00b7} ? help \u{00b7} q quit".to_string()
        };
        self.status.set_text(&status);
        self.status.refresh();
    }

    fn help(&mut self) {
        let content = "\
 petri \u{2014} keys

  Tab                switch focus (places / transitions)
  j k / arrows       move selection
  Enter / f          fire the selected transition
  Space              auto-run: fire random enabled transitions
  + / -              add / remove a token (places pane)
  u                  undo last firing
  R                  reset to the file's initial marking
  a                  analyze reachable markings
                     (deadlocks, boundedness, place bounds)
  e                  edit the net file in $EDITOR, reload
  ? h                this help
  q                  quit

 Net file format \u{2014} two line shapes, all else ignored:
   lock = 1                    place with initial tokens
   enter: idle lock -> crit    transition (repeat = weight)

 \u{203c} marks a conflict: enabled transitions competing for
 the same tokens. The net cannot decide. You do.

 (Esc / q / Enter to close)";
        let (cols, rows) = Crust::terminal_size();
        let lines = content.lines().count() as u16 + 1;
        let w = 60.min(cols.saturating_sub(2)).max(20);
        let h = lines.min(rows.saturating_sub(2)).max(3);
        let mut pop = Popup::centered(w, h, 231, C_HEADER_BG);
        pop.view(content);
        self.after_popup();
    }

    fn analyze_popup(&mut self) {
        let a = analyze(&self.net, &self.marking, 50_000);
        let content = analysis_text(&self.net, &a);
        let (cols, rows) = Crust::terminal_size();
        let lines = content.lines().count() as u16 + 1;
        let w = 64.min(cols.saturating_sub(2)).max(20);
        let h = lines.min(rows.saturating_sub(2)).max(3);
        let mut pop = Popup::centered(w, h, 250, 233);
        pop.view(&content);
        self.after_popup();
    }

    fn after_popup(&mut self) {
        Crust::clear_screen();
        for p in [&mut self.header, &mut self.places, &mut self.trans, &mut self.logp, &mut self.status] {
            p.invalidate();
        }
        self.render();
    }

    /// Edit the net file in $EDITOR (sync TTY handoff), then reload.
    fn edit(&mut self) {
        let Some(file) = self.file.clone() else {
            self.msg = Some(" no file to edit (started with builtin demo)".to_string());
            return;
        };
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string());
        Crust::cleanup();
        let _ = std::process::Command::new(&editor).arg(&file).status();
        Crust::init();
        Crust::set_app_identity("Petri");
        Crust::clear_screen();
        match std::fs::read_to_string(&file).map_err(|e| e.to_string()).and_then(|t| Net::parse(&t)) {
            Ok(net) => {
                self.net = net;
                self.marking = self.net.initial_marking();
                self.undo.clear();
                self.steps = 0;
                self.pidx = 0;
                self.tidx = 0;
                self.push_log("net reloaded".to_string());
            }
            Err(e) => self.msg = Some(format!(" reload failed: {}", e)),
        }
        self.layout();
        self.render();
    }

    /// Auto-run: fire a random enabled transition every tick until a key
    /// is pressed or the net deadlocks.
    fn run_loop(&mut self) {
        use crossterm::event::{self, Event, KeyEventKind};
        use std::time::Duration;
        self.running = true;
        self.render();
        loop {
            if event::poll(Duration::from_millis(250)).unwrap_or(false) {
                if let Ok(Event::Key(k)) = event::read() {
                    if k.kind == KeyEventKind::Press {
                        break;
                    }
                }
            } else {
                let en = self.net.enabled_set(&self.marking);
                if en.is_empty() {
                    break;
                }
                let t = en[self.rng.next(en.len())];
                self.fire(t);
                self.render();
            }
        }
        self.running = false;
        self.render();
    }

    fn handle(&mut self, key: &str) -> bool {
        match key {
            "q" => return false,
            "TAB" => {
                self.focus = if self.focus == Focus::Places { Focus::Trans } else { Focus::Places };
            }
            "j" | "DOWN" => match self.focus {
                Focus::Places => self.pidx = (self.pidx + 1).min(self.net.places.len().saturating_sub(1)),
                Focus::Trans => self.tidx = (self.tidx + 1).min(self.net.trans.len().saturating_sub(1)),
            },
            "k" | "UP" => match self.focus {
                Focus::Places => self.pidx = self.pidx.saturating_sub(1),
                Focus::Trans => self.tidx = self.tidx.saturating_sub(1),
            },
            "ENTER" | "f" => {
                if self.focus == Focus::Trans {
                    self.fire(self.tidx);
                }
            }
            " " => self.run_loop(),
            "+" | "=" => {
                if self.focus == Focus::Places {
                    self.undo.push(self.marking.clone());
                    self.marking[self.pidx] += 1;
                }
            }
            "-" => {
                if self.focus == Focus::Places && self.marking[self.pidx] > 0 {
                    self.undo.push(self.marking.clone());
                    self.marking[self.pidx] -= 1;
                }
            }
            "u" => {
                if let Some(m) = self.undo.pop() {
                    self.marking = m;
                    self.steps = self.steps.saturating_sub(1);
                    self.push_log("undo".to_string());
                } else {
                    self.msg = Some(" nothing to undo".to_string());
                }
            }
            "R" => {
                self.undo.push(self.marking.clone());
                self.marking = self.net.initial_marking();
                self.steps = 0;
                self.push_log("reset to initial marking".to_string());
            }
            "a" => self.analyze_popup(),
            "e" => self.edit(),
            "?" | "h" => self.help(),
            _ => {}
        }
        true
    }
}

const DEMO: &str = include_str!("../examples/mutex.net");

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Headless: petri --analyze <file>  (no TUI; for scripting/CI)
    if args.get(1).map(|s| s.as_str()) == Some("--analyze") {
        let Some(path) = args.get(2) else {
            eprintln!("usage: petri --analyze <file.net>");
            std::process::exit(1);
        };
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("petri: cannot read {}: {}", path, e);
                std::process::exit(1);
            }
        };
        match Net::parse(&text) {
            Ok(net) => {
                let a = analyze(&net, &net.initial_marking(), 200_000);
                println!(
                    "{}: {} places, {} transitions",
                    path,
                    net.places.len(),
                    net.trans.len()
                );
                print!("{}", analysis_text(&net, &a).replace("\n (Esc/q to close)", ""));
                std::process::exit(if a.deadlock_count > 0 { 2 } else { 0 });
            }
            Err(e) => {
                eprintln!("petri: parse error in {}: {}", path, e);
                std::process::exit(1);
            }
        }
    }

    // TUI: parse before init so errors print normally.
    let (net, file) = match args.get(1) {
        Some(p) => {
            let text = match std::fs::read_to_string(p) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("petri: cannot read {}: {}", p, e);
                    std::process::exit(1);
                }
            };
            match Net::parse(&text) {
                Ok(n) => (n, Some(p.clone())),
                Err(e) => {
                    eprintln!("petri: parse error in {}: {}", p, e);
                    std::process::exit(1);
                }
            }
        }
        None => (Net::parse(DEMO).expect("builtin demo parses"), None),
    };

    Crust::init();
    Crust::set_app_identity("Petri");

    let mut app = App::new(net, file);
    app.render();

    loop {
        let key = Input::getchr(None).unwrap_or_default();
        if key == "RESIZE" {
            app.layout();
            app.render();
            continue;
        }
        if !app.handle(&key) {
            break;
        }
        app.render();
    }

    Crust::cleanup();
}
