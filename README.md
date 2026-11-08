# Tasker

A desktop task planner and time tracker for Linux and Windows, written in Rust with [egui](https://github.com/emilk/egui).

```
┌ title bar ────────────────────────────────────────────── ☰  ⚙  🗕  🗖  ❌ ┐
│ ◀ November 2026 ▶ │00:00│ ╲    │ Thursday, 05 November 2026            │
│  month calendar   │     │  ╲___│ Email triage   done   08:00 – 08:30   │
│───────────────────│06:00│     ╱│────────────────────────────────────────│
│ Monday       2    │     │ ___╱ │ Write report   paused  ⏱ 02:05:00     │
│ Tuesday      3    │12:00│╱     │────────────────────────────────────────│
│ …   (week list)   │     │      │ …   (tasks of the selected day)        │
│ [ + Add task ]    │24:00│      │                                        │
└───────────────────┴─────┴──────┴────────────────────────────────────────┘
```

## Build & run

```sh
cargo build-all             # release builds for Linux and Windows -> binary/tasker, binary/tasker.exe
binary/tasker               # run it
binary/tasker --minimized   # start in the panel (e.g. for autostart)
binary/tasker --test-sound  # play the configured reminder sound, report errors
```

`cargo build-all` builds the OS it runs on natively and cross-compiles the other one. It
works on Linux and Windows and needs a cross toolchain once (missing Rust targets are added
automatically):

| Building on | One-time setup for the other OS's binary |
|-------------|------------------------------------------|
| Linux       | MinGW: `sudo apt install gcc-mingw-w64-x86-64` (Fedora: `mingw64-gcc`) |
| Windows     | zig: `winget install zig.zig`, then `cargo install --locked cargo-zigbuild` |

On Windows, Rust itself needs the MSVC build tools (rustup's installer offers them). If the
cross toolchain is missing, the native binary is still built and copied, and the command
fails with the install hint. `binary/` is ignored by git.

Plain `cargo build`, `cargo run` and `cargo test` work as usual and only build for the
current OS (`target/debug` or `target/release`). `build-all` is a cargo alias
(`.cargo/config.toml`) for the small build program in `xtask/`: cargo has no post-build
step, and an alias can't take over `cargo build` itself.

**Linux:** no system `-dev` packages are needed. At runtime Tasker uses X11 (XWayland on Wayland
sessions), D-Bus for the panel icon and `libasound.so.2` for sound. All of these are
present on a normal desktop install.

**Portability:** the binary runs on glibc 2.34 or newer (Linux Mint 21+, Ubuntu 22.04+,
Debian 12+), even when built on a newer distro. `build.rs` + `build/glibc_compat.c` pin a few
libm functions to their old symbol versions. When built on Windows, zig links against
glibc 2.34 directly.

Data lives in `~/.local/share/tasker/tasker.db` (SQLite; Windows: see below). To back it up, copy that file while
Tasker isn't running (while it runs, recent changes may still be in `tasker.db-wal`). Reminder
state (dismissed/snoozed) older than a week is cleaned up at startup.

A log (start/exit, errors, reminder events, crashes with backtrace) is written to
`~/.local/state/tasker/tasker.log` (moved to `tasker.log.1` at startup once it exceeds 1 MB). Release builds keep line tables, so
crash backtraces show file and line. Starting a second instance only brings the running one
to the front.

| Environment variable   | Effect                                                                 |
|------------------------|------------------------------------------------------------------------|
| `TASKER_DB`            | Use another database file (also gets its own single-instance socket)   |
| `TASKER_WAYLAND=1`     | Use native Wayland instead of XWayland (notifications can't be placed) |
| `TASKER_ALSA_DEVICE`   | ALSA output device (default `default`, e.g. `pulse`, `pipewire`, `hw:0`) |

**eframe patch:** `vendor/eframe` is eframe 0.37.0 with a one-line fix (opening a reminder
while the main window is hidden panicked), wired in through `[patch.crates-io]` in
`Cargo.toml`. See `vendor/eframe/README.md`; drop it once an eframe release has the fix.

### Windows

`binary/tasker.exe` is a single file that needs nothing besides Windows 10/11 itself: copy it
anywhere and start it. It has no console window; `tasker.exe --test-sound` from a terminal
still prints its result. Windows may warn about an unknown publisher on the first start (the
exe isn't signed). `build.rs` embeds the icon and version info; it finds `rc.exe` in the
Windows SDK (or `RC`), MinGW's `windres` (or `WINDRES`) when cross-compiling. Without one the
build prints a warning and the exe has a generic file icon; the app itself is unaffected. For autostart, put a shortcut with `--minimized` into the folder that
`Win+R` → `shell:startup` opens.

What differs from Linux: the panel icon is a normal notification-area icon (left click
shows / hides, right click opens the menu), sound goes to the default output device, the
data lives in `%APPDATA%\tasker\tasker.db` and the log in `%LOCALAPPDATA%\tasker\tasker.log`.
`TASKER_WAYLAND` and `TASKER_ALSA_DEVICE` don't apply.

## Concepts

* **Virtual task**: the plan you create and edit: title, notes, start time, estimate,
  priority, reminder mode and schedule. It never becomes "done" itself. Recurring ones
  appear endlessly in the calendar, week list and day view.
* **Real task (run)**: created only when you press **Start task** on one occurrence. It
  snapshots the plan, so editing the virtual task later changes upcoming occurrences only.
  Time is tracked in segments (start/pause/continue). **Finish** greys the task out, and
  **Resume** reopens it.
* **Earlier work**: while a task runs, *+1 min / +10 min / +1 hour* add time you already
  worked on it before pressing Start. It is kept apart from the measured time, so *Reset*
  removes only the added part. It also moves "started … early/late" accordingly.
* **Day view**: each task shows how far its start is away ("starts in 25 min", "12 min
  overdue") or how punctually it was started, and when a snoozed reminder comes back. The
  timeline on the left marks every task's time span; overlapping tasks get separate lanes.
* **Schedules**
  * *A date*: once, or every year on that date (29 Feb falls back to 28 Feb).
  * *Weekdays* (any combination): every week, every 2 weeks (counted from the start
    date's week), or only the first or last of those weekdays in each month.
  * *Month days* (any combination): every month. Months without that day are skipped.
  * Recurring schedules begin on a start date. Its calendar only allows days that match
    the selection. Weekly/monthly single dates from older versions are converted on load.
* **Task list**: the ☰ button in the title bar lists all recurring tasks (schedule, time,
  estimate, priority color; by start time) and all one-off tasks that haven't been started
  yet (by date; past ones are marked), each with an Edit button.
* **Statistics** (recurring tasks, 📊): the last 30 occurrences with tracked time against the
  estimate, how many were started / done / over the estimate, and the average start delay.
* **Edits and history**: a started occurrence keeps the title, estimate etc. it had when
  it was started. The statistics show the current estimate as a solid line and, where it
  differs, the estimate at the time as a dashed line.
* **UI scale**: Settings → UI scale (50–200 %), or Ctrl + / Ctrl − / Ctrl 0. It is saved.
* **Window size**: the main window remembers its position, size and maximized state. The
  first start opens it at 1280×800, which is also the minimum size. Both grow with the UI
  scale but never beyond the screen. A window whose saved place is off-screen (e.g. a
  monitor was removed) is centered on the primary monitor.
* **Keyboard**: Ctrl + Enter saves the task editor, Esc closes the topmost dialog.

## Reminders

* A reminder pops up when a task's start time passes and it hasn't been started yet. A
  second kind appears when a running task exceeds its estimate (added earlier work counts);
  it has Pause / Finish and a Dismiss button instead of snoozing.
* **Notify ahead** (per task, in minutes): a short announcement that the task starts soon.
  It has only a Dismiss button, plays no sound and closes by itself after 10 seconds.
* Reminders are drawn by Tasker itself (not the system's notification service), in the
  configured screen corner, stacked and always on top. The longer they are overdue, the
  redder they get (full red after 60 min).
* They offer the same buttons as the task (start / pause / finish / resume). Clicking one
  closes the reminder after a second. A due reminder has no close button: it stays until
  the task is started, and only the snooze buttons (default 5 / 10 / 30 min, 1 h, 2 h) put
  it off. Over-estimate reminders and announcements can be dismissed. Snoozing and
  dismissing are saved, so a restart doesn't bring them back.
* Sound loops while a reminder is open, with a volume ramp (start volume → end volume over
  N seconds). Choose a built-in sound (Chime, Bell, Beep) or a WAV/MP3/OGG/FLAC file. A
  broken file falls back to the chime.

## Desktop support

| Desktop                    | Panel icon                                   | Corner reminders |
|----------------------------|----------------------------------------------|------------------|
| Linux Mint / Cinnamon      | ✔ (xapp status icons)                        | ✔                |
| KDE Plasma, XFCE, MATE, LXQt | ✔                                          | ✔                |
| GNOME                      | needs the *AppIndicator* extension (Ubuntu ships it) | ✔ via XWayland |
| Sway / Hyprland            | ✔ with waybar's tray                         | ✔ via XWayland   |
| Windows 10 / 11            | ✔ (notification area)                        | ✔                |

Without a panel host, "minimize" falls back to a normal minimize so the window can't get lost.

**Panel icon:** a click shows or hides the window. The icon is blue when idle, yellow while
a task is paused and green while one is running (green wins). Its right-click menu lists
the running and paused tasks with Pause / Resume / Finish, then Show / hide and Quit. In
the settings, the window's close button can minimize to the panel instead of quitting, and
Tasker can start minimized there (`--minimized` does the same once).

## Code map

| File | Contents |
|------|----------|
| `src/main.rs`     | Startup: single instance, window options, `--minimized` / `--test-sound` |
| `src/model.rs`    | Tasks, schedules, runs, per-day entries, formatting |
| `src/store.rs`    | SQLite persistence, migrations, in-memory model, time tracking |
| `src/app.rs`      | App state, window chrome and geometry, which reminders to show, reminder windows |
| `src/views.rs`    | Calendar, week list, 24h timeline, day's task rows, connector lines |
| `src/dialogs.rs`  | Task editor, task list, statistics, settings |
| `src/settings.rs` | Settings and their defaults, UI scale steps, saved window geometry |
| `src/audio.rs`    | Sound synthesis/decoding, looping playback with volume ramp |
| `src/logfile.rs`  | Log file, panic hook, crash (signal) handler on Linux |
| `src/tray.rs`, `src/icon.rs` | Panel icon contents (color, tooltip, menu), generated icon |
| `src/platform/`   | Everything OS-specific, as `linux/` and `windows/` with the same names: screen work areas (X11 / Win32), panel icon (D-Bus / notification area), single instance (Unix socket / localhost port), sound output (ALSA / waveOut), file chooser (zenity, kdialog / Windows dialog) |
| `build.rs`        | Linux: glibc symbol pinning. Windows: icon and version info in the .exe |
| `xtask/`          | `cargo build-all`: native + cross build, copies the binaries to `binary/` |

## Tests

```sh
cargo test                  # schedules, store, reminders, editor, statistics, settings, tray
cargo test --release frame_queries -- --ignored --nocapture   # per-frame cost with 3 years of history
```

Most modules have tests at the bottom. The reminder logic (`wanted_reminders` in `app.rs`)
is a plain function of the store and the time, so new reminder rules can be tested without
a display.

In VS Code, Ctrl+Shift+B runs `cargo build-all`; "Tasks: Run Task" also offers a debug build
(`.vscode/tasks.json`).
