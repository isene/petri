# Changelog

## 0.1.0

Initial release.

- Plain-text net format (places with initial marking, transitions with
  weighted arcs via repetition); HyperList-shaped files parse as-is.
- TUI: places pane with token dots, transitions pane with enabled (`▶`) and
  conflict (`‼`) markers, firing log, deadlock banner.
- Fire by hand, auto-run with random conflict resolution, undo, reset,
  live token editing (`+`/`-`).
- Reachability analysis (TUI popup and headless `--analyze`): deadlocks with
  example dead markings, boundedness, per-place bounds. Exit code 2 on
  reachable deadlock for scripting.
- `e` edits the net in `$EDITOR` and reloads on return.
- Examples: mutex, producer/consumer, dining philosophers.

## 0.1.1

- Layout rework after real-screen feedback: compact bordered panes sized to
  the net (no more content scattered across a huge terminal), focused pane
  shown by border colour, high-contrast selection bar, empty places shown
  as `·`, and the trace pane (titled, explained) fills the remaining height.
