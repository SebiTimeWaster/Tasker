//! Small X11 helpers that winit/egui don't offer: the usable screen area of the primary
//! monitor (for placing notifications in a corner) and hiding notification windows from
//! the taskbar / window list.

use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, Window};
use x11rb::rust_connection::RustConnection;

pub struct X11 {
    conn: RustConnection,
    root: Window,
    atoms: Atoms,
}

/// The atoms we use, interned once (each lookup is a round trip to the X server).
struct Atoms {
    net_wm_name: u32,
    utf8_string: u32,
    net_client_list: u32,
    net_workarea: u32,
    net_current_desktop: u32,
    net_wm_state: u32,
    skip_taskbar: u32,
    skip_pager: u32,
    above: u32,
}

use crate::platform::Area;

/// Raw monitor data: RandR monitors (primary first), the current desktop and the work
/// areas the window manager publishes for it.
struct Geometry {
    monitors: Vec<Area>,
    desktop: usize,
    per_monitor: Option<Vec<Area>>,
    whole: Option<Area>,
}

impl X11 {
    pub fn connect() -> Option<Self> {
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots[screen].root;
        // Send all requests first, then collect the replies: one round trip in total.
        let names = [
            "_NET_WM_NAME",
            "UTF8_STRING",
            "_NET_CLIENT_LIST",
            "_NET_WORKAREA",
            "_NET_CURRENT_DESKTOP",
            "_NET_WM_STATE",
            "_NET_WM_STATE_SKIP_TASKBAR",
            "_NET_WM_STATE_SKIP_PAGER",
            "_NET_WM_STATE_ABOVE",
        ];
        let cookies: Vec<_> =
            names.iter().map(|n| conn.intern_atom(false, n.as_bytes())).collect::<Result<_, _>>().ok()?;
        let a: Vec<u32> = cookies.into_iter().map(|c| c.reply().map(|r| r.atom)).collect::<Result<_, _>>().ok()?;
        let atoms = Atoms {
            net_wm_name: a[0],
            utf8_string: a[1],
            net_client_list: a[2],
            net_workarea: a[3],
            net_current_desktop: a[4],
            net_wm_state: a[5],
            skip_taskbar: a[6],
            skip_pager: a[7],
            above: a[8],
        };
        Some(X11 { conn, root, atoms })
    }

    fn cardinals(&self, win: Window, atom: u32, ty: impl Into<u32>) -> Option<Vec<u32>> {
        let reply = self.conn.get_property(false, win, atom, ty.into(), 0, 4096).ok()?.reply().ok()?;
        Some(reply.value32()?.collect())
    }

    /// Usable area of the primary monitor (panels excluded).
    pub fn work_area(&self) -> Option<Area> {
        self.monitor_areas().into_iter().next()
    }

    /// Every monitor's usable area (panels excluded); primary first.
    pub fn monitor_areas(&self) -> Vec<Area> {
        let Some(g) = self.geometry() else { return vec![] };
        monitor_work_areas(&g.monitors, g.per_monitor.as_deref(), g.whole)
    }

    /// What the X server and window manager report about the monitors, for the log.
    pub fn describe(&self) -> String {
        let Some(g) = self.geometry() else { return "no RandR monitors".into() };
        format!(
            "monitors {:?} (primary first), desktop {}, _GTK_WORKAREAS {:?}, _NET_WORKAREA {:?}",
            g.monitors, g.desktop, g.per_monitor, g.whole
        )
    }

    fn geometry(&self) -> Option<Geometry> {
        let monitors = self.conn.randr_get_monitors(self.root, true).ok()?.reply().ok()?;
        let mut mons: Vec<_> = monitors.monitors.iter().collect();
        mons.sort_by_key(|m| !m.primary);
        let monitors = mons.iter().map(|m| (m.x as i32, m.y as i32, m.width as i32, m.height as i32)).collect();
        let desktop = self
            .cardinals(self.root, self.atoms.net_current_desktop, AtomEnum::CARDINAL)
            .and_then(|v| v.first().copied())
            .unwrap_or(0) as usize;
        let rects = |v: Vec<u32>| -> Vec<Area> {
            v.as_chunks::<4>().0.iter().map(|&[x, y, w, h]| (x as i32, y as i32, w as i32, h as i32)).collect()
        };
        // _NET_WORKAREA is one rectangle per desktop spanning all monitors, so with several
        // monitors it is wrong for some of them (a panel on one monitor or monitors of
        // different heights shift it). Mutter, Muffin & co. also publish the exact work
        // area of each monitor as _GTK_WORKAREAS_D<desktop>; prefer that.
        let per_monitor = self.gtk_workareas(desktop).map(rects);
        let whole = self
            .cardinals(self.root, self.atoms.net_workarea, AtomEnum::CARDINAL)
            .map(rects)
            .and_then(|r| r.get(desktop).or(r.first()).copied());
        Some(Geometry { monitors, desktop, per_monitor, whole })
    }

    fn gtk_workareas(&self, desktop: usize) -> Option<Vec<u32>> {
        let name = format!("_GTK_WORKAREAS_D{desktop}");
        // only_if_exists: don't create the atom when the window manager doesn't use it
        let atom = self.conn.intern_atom(true, name.as_bytes()).ok()?.reply().ok()?.atom;
        if atom == 0 {
            return None;
        }
        self.cardinals(self.root, atom, AtomEnum::CARDINAL).filter(|v| v.len() >= 4)
    }

    fn window_title(&self, win: Window) -> Option<String> {
        let r = self
            .conn
            .get_property(false, win, self.atoms.net_wm_name, self.atoms.utf8_string, 0, 1024)
            .ok()?
            .reply()
            .ok()?;
        String::from_utf8(r.value).ok()
    }

    /// Find a mapped top-level window by its exact title and mark it skip-taskbar /
    /// skip-pager. Returns false if it isn't mapped yet (retry next frame).
    pub fn hide_from_taskbar(&self, title: &str) -> bool {
        let Some(list) = self.cardinals(self.root, self.atoms.net_client_list, AtomEnum::WINDOW) else {
            return false;
        };
        // Newest windows are at the end of the list; ours was just created.
        let Some(win) = list.into_iter().rev().find(|&w| self.window_title(w).as_deref() == Some(title)) else {
            return false;
        };
        let a = &self.atoms;
        let (state, skip_tb, skip_pg, above) = (a.net_wm_state, a.skip_taskbar, a.skip_pager, a.above);
        let mask = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
        for (a, b) in [(skip_tb, skip_pg), (above, 0)] {
            // _NET_WM_STATE_ADD = 1, source indication 1 = normal application
            let ev = ClientMessageEvent::new(32, win, state, [1, a, b, 1, 0]);
            let _ = self.conn.send_event(false, self.root, mask, ev);
        }
        let _ = self.conn.flush();
        true
    }
}

fn intersect(a: Area, b: Area) -> Option<Area> {
    let (x0, y0) = (a.0.max(b.0), a.1.max(b.1));
    let (x1, y1) = ((a.0 + a.2).min(b.0 + b.2), (a.1 + a.3).min(b.1 + b.3));
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
}

/// The usable part of each monitor: the per-monitor work area that lies on it if the
/// window manager publishes those (`per_monitor`), else the monitor clipped to the
/// desktop-wide work area (`whole`), else the whole monitor.
fn monitor_work_areas(monitors: &[Area], per_monitor: Option<&[Area]>, whole: Option<Area>) -> Vec<Area> {
    let big_enough = |a: &Area| a.2 > 100 && a.3 > 100;
    monitors
        .iter()
        .map(|&mon| {
            let own = per_monitor.and_then(|rs| {
                rs.iter().filter_map(|&r| intersect(mon, r)).filter(big_enough).max_by_key(|r| r.2 as i64 * r.3 as i64)
            });
            own.or_else(|| whole.and_then(|w| intersect(mon, w)).filter(big_enough)).unwrap_or(mon)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::monitor_work_areas;

    // Primary 1920x1080 at the left, a taller 2560x1440 monitor at its right, tops aligned.
    const PRIMARY: (i32, i32, i32, i32) = (0, 0, 1920, 1080);
    const SECOND: (i32, i32, i32, i32) = (1920, 0, 2560, 1440);

    #[test]
    fn per_monitor_work_areas_win() {
        // Panel at the bottom of the second monitor only. The desktop-wide work area
        // (bounding box minus the strut) would cut 40 px off the primary monitor too.
        let per = [(0, 0, 1920, 1080), (1920, 0, 2560, 1400)];
        let whole = Some((0, 0, 4480, 1040));
        assert_eq!(monitor_work_areas(&[PRIMARY, SECOND], Some(&per), whole), [PRIMARY, (1920, 0, 2560, 1400)]);
        // listed in another order than the monitors: matched by position
        let per = [(1920, 0, 2560, 1400), (0, 0, 1920, 1040)];
        assert_eq!(
            monitor_work_areas(&[PRIMARY, SECOND], Some(&per), whole),
            [(0, 0, 1920, 1040), (1920, 0, 2560, 1400)]
        );
    }

    #[test]
    fn smaller_monitor_left_of_the_primary() {
        // A smaller monitor on the left, tops aligned; the taller primary on the right
        // with the panel at its bottom. The screen is L-shaped, so Muffin's
        // _NET_WORKAREA is the largest rectangle inside it: only 1080 px high. Clipping
        // the primary monitor to it put reminders at the small monitor's bottom edge.
        let left = (0, 0, 1920, 1080);
        let primary = (1920, 0, 2560, 1440);
        let whole = Some((0, 0, 4480, 1080));
        assert_eq!(monitor_work_areas(&[primary, left], None, whole)[0], (1920, 0, 2560, 1080), "the old bug");
        let per = [(0, 0, 1920, 1080), (1920, 0, 2560, 1400)];
        assert_eq!(monitor_work_areas(&[primary, left], Some(&per), whole), [(1920, 0, 2560, 1400), left]);
    }

    #[test]
    fn falls_back_to_the_desktop_wide_work_area() {
        let whole = Some((0, 0, 4480, 1040));
        assert_eq!(monitor_work_areas(&[PRIMARY, SECOND], None, whole), [(0, 0, 1920, 1040), (1920, 0, 2560, 1040)]);
        assert_eq!(monitor_work_areas(&[PRIMARY], None, None), [PRIMARY]);
        // a work area that misses a monitor leaves the monitor whole
        assert_eq!(monitor_work_areas(&[SECOND], None, Some((0, 0, 1920, 1040))), [SECOND]);
    }
}
