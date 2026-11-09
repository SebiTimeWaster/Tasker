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
    net_wm_state: u32,
    skip_taskbar: u32,
    skip_pager: u32,
    above: u32,
}

use crate::platform::Area;

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
            net_wm_state: a[4],
            skip_taskbar: a[5],
            skip_pager: a[6],
            above: a[7],
        };
        Some(X11 { conn, root, atoms })
    }

    fn cardinals(&self, win: Window, atom: u32, ty: impl Into<u32>) -> Option<Vec<u32>> {
        let reply = self.conn.get_property(false, win, atom, ty.into(), 0, 4096).ok()?.reply().ok()?;
        Some(reply.value32()?.collect())
    }

    /// Primary monitor intersected with the window manager's work area (excludes panels).
    pub fn work_area(&self) -> Option<Area> {
        self.monitor_areas().into_iter().next()
    }

    /// Every monitor intersected with the window manager's work area; primary first.
    pub fn monitor_areas(&self) -> Vec<Area> {
        let Some(monitors) = self.conn.randr_get_monitors(self.root, true).ok().and_then(|c| c.reply().ok()) else {
            return vec![];
        };
        let work = self
            .cardinals(self.root, self.atoms.net_workarea, AtomEnum::CARDINAL)
            .filter(|wa| wa.len() >= 4)
            .map(|wa| (wa[0] as i32, wa[1] as i32, wa[2] as i32, wa[3] as i32));
        let mut mons: Vec<_> = monitors.monitors.iter().collect();
        mons.sort_by_key(|m| !m.primary);
        mons.into_iter()
            .map(|mon| {
                let area = (mon.x as i32, mon.y as i32, mon.width as i32, mon.height as i32);
                let Some((wx, wy, ww, wh)) = work else { return area };
                let x0 = area.0.max(wx);
                let y0 = area.1.max(wy);
                let x1 = (area.0 + area.2).min(wx + ww);
                let y1 = (area.1 + area.3).min(wy + wh);
                if x1 - x0 > 100 && y1 - y0 > 100 { (x0, y0, x1 - x0, y1 - y0) } else { area }
            })
            .collect()
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
