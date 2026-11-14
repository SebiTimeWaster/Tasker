# Tasker

Tasker is a task app for people who know exactly what they should do and still don't do it,
whether that's ADHD, executive dysfunction or a brain that simply refuses to start. Instead
of a polite notification that's easy to swipe away, Tasker puts a reminder on top of all
your windows and keeps ringing until you either start the task or admit, in so many words,
that you won't do it. It is not a calendar for keeping track of appointments: it is for the
tasks you keep putting off, and it is meant to be annoying about them.

## Features

**Reminders that don't go away**

* When a task's start time passes, a reminder pops up in a screen corner, always on top of
  other windows, and plays a sound that loops and gets louder until you act.
* There is no close button. You can start the task, snooze it once (5 min to 2 h), or give
  it up. A snoozed reminder comes back without snooze buttons.
* Giving up means clicking **I am bad and won't do it**, and that button only unlocks after
  the reminder has been open for 10 seconds.
* The line saying how late you are turns redder the longer you wait.
* A second reminder appears when a running task takes longer than you estimated.
* An optional "starts soon" heads-up before a task begins.
* Reminders survive a restart, and a task you missed yesterday keeps reminding you until the
  end of today.

**Planning**

* One-off tasks and recurring ones: every year on a date, on chosen weekdays (every week,
  every 2 weeks, or the first or last week of a month) or on chosen days of the month.
* Each task has a start time, an estimate (up to almost two days), a priority and notes.
* A month calendar, a week list and a day view with a 24-hour timeline. Tasks that run past
  midnight show up on both days.
* A list of all recurring tasks and all upcoming one-off tasks.
* Configurable defaults for new tasks and preset buttons for common times and durations.

**Time tracking**

* Start, pause, continue, finish and resume a task. The tracked time is shown live.
* Add time you already spent before pressing Start (+1 min, +10 min, +1 hour).
* The day view shows how soon each task starts, how overdue it is, or how punctually you
  started it.
* Clicked Start or "won't do it" by accident? **Undo** is available in the main window for 60 seconds.

**Honest statistics**

* For each recurring task: the last 30 occurrences with tracked time against the estimate,
  how many were started, done, over the estimate, given up or missed, and your average start
  delay.

**On your desktop**

* A panel icon that shows whether a task is running (green), paused (yellow) or nothing is
  going on (blue), with a right-click menu to pause, resume or finish tasks without opening the window.
* Closes to the panel instead of quitting, and can start minimized.
* Built-in reminder sounds (chime, bell, beep) or your own WAV, MP3, OGG or FLAC file, with
  adjustable start and end volume.
* UI scale from 50 to 200 %.
* Everything stays on your computer in a single local database: no account, no cloud.
* Runs on Linux (Cinnamon, KDE Plasma, XFCE, MATE, LXQt, GNOME with the AppIndicator
  extension, Sway and Hyprland) and on Windows 10 and 11.

## Build and install

> **Tasker is still alpha.** Expect rough edges and changes between versions. Proper
> releases are coming soon; until then, download a development build or build it yourself.

### Development builds

Every change to the code is built automatically. Open the latest successful run on the
[CI page](https://github.com/SebiTimeWaster/Tasker/actions/workflows/ci.yml) and download
`tasker-linux` or `tasker-windows` under *Artifacts* (this needs a GitHub account).

* **Linux:** unpack it and make it executable with `chmod +x tasker`. It runs on Linux Mint
  21+, Ubuntu 22.04+, Debian 12+ and other distributions with glibc 2.34 or newer.
* **Windows:** `tasker.exe` needs nothing else. Windows may warn about an unknown publisher
  on the first start, because the file isn't signed.

### Building it yourself

1. Install Rust with [rustup](https://rustup.rs). On Windows, let the installer add the MSVC
   build tools. On Linux, no extra development packages are needed.
2. Get the code and build it:

   ```sh
   git clone https://github.com/SebiTimeWaster/Tasker.git
   cd Tasker
   cargo build --release
   ```

3. The app is now `target/release/tasker` (`tasker.exe` on Windows). It is a single file, so
   copy it wherever you like and start it.

### Starting at login

To start Tasker at login, add it to your desktop's autostart (on Windows: put a shortcut into
the folder that `Win+R` → `shell:startup` opens) with the `--minimized` option, so it starts
in the panel.

More details on building and platform support are in [DEVELOPMENT.md](DEVELOPMENT.md).

## Your data

Tasker keeps all your tasks, tracked time and settings in a single file on your computer:

* Linux: `~/.local/share/tasker/tasker.db`
* Windows: `%APPDATA%\tasker\tasker.db`

To back it up, quit Tasker and copy that file. To restore it, quit Tasker and put the copy
back.

## Contributing

Any contribution is welcome! That includes bug reports, ideas, feedback on what works for
you and what doesn't (especially if you are one of the people Tasker is made for), help with
the wording, and code. Open an [issue](https://github.com/SebiTimeWaster/Tasker/issues) or
send a pull request. [DEVELOPMENT.md](DEVELOPMENT.md) explains how the code is organized and
how to run the tests, and lists planned work.

## License

Tasker is released under the [MIT License](LICENSE).
