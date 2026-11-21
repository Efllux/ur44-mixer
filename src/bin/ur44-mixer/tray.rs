//! Menu bar icon + menu, and the control channel used by the launcher ("show the window").

use iced::futures::{SinkExt, Stream, channel::mpsc, executor::block_on};
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixListener;
use std::sync::OnceLock;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

#[derive(Debug, Clone)]
pub enum Control {
    Show,
    ShowSettings,
    RecallScene(usize),
    Quit,
}

/// The listening socket is bound in main() (before the UI starts) and handed to the stream.
pub static LISTENER: OnceLock<UnixListener> = OnceLock::new();

pub struct Tray {
    icon: TrayIcon,
    status: MenuItem,
    last_status: String,
    open_id: MenuId,
    settings_id: MenuId,
    quit_id: MenuId,
    scene_ids: Vec<(MenuId, usize)>,
}

/// 18×18 template image: three mixer faders (black + alpha; macOS tints it for light/dark).
fn icon_image() -> Icon {
    let (w, h) = (18u32, 18u32);
    let mut px = vec![0u8; (w * h * 4) as usize];
    let mut set = |x: u32, y: u32, a: u8| {
        if x < w && y < h {
            let i = ((y * w + x) * 4) as usize;
            px[i + 3] = px[i + 3].max(a);
        }
    };
    for (col, knob) in [(4u32, 11u32), (9, 5), (14, 9)] {
        for y in 2..16 {
            set(col, y, 160);
        }
        for dy in 0..3 {
            for dx in 0..5 {
                set(col + dx - 2, knob + dy - 1, 255);
            }
        }
    }
    Icon::from_rgba(px, w, h).expect("valid icon")
}

impl Tray {
    pub fn new(scenes: &[String]) -> Result<Tray, String> {
        let status = MenuItem::new("UR44 \u{2014} starting\u{2026}", false, None);
        let (menu, open_id, settings_id, quit_id, scene_ids) = build_menu(&status, scenes)?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("UR44 Mixer")
            .with_icon_templated(icon_image())
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Tray { icon, status, last_status: String::new(), open_id, settings_id, quit_id, scene_ids })
    }

    pub fn set_status(&mut self, s: &str) {
        if self.last_status != s {
            self.status.set_text(s);
            self.last_status = s.to_string();
        }
    }

    pub fn set_scenes(&mut self, scenes: &[String]) {
        let status = MenuItem::new(self.last_status.clone(), false, None);
        if let Ok((menu, open_id, settings_id, quit_id, scene_ids)) = build_menu(&status, scenes) {
            self.icon.set_menu(Some(Box::new(menu)));
            self.status = status;
            self.open_id = open_id;
            self.settings_id = settings_id;
            self.quit_id = quit_id;
            self.scene_ids = scene_ids;
        }
    }

    pub fn translate(&self, id: &MenuId) -> Option<Control> {
        if *id == self.open_id {
            Some(Control::Show)
        } else if *id == self.settings_id {
            Some(Control::ShowSettings)
        } else if *id == self.quit_id {
            Some(Control::Quit)
        } else {
            self.scene_ids.iter().find(|(m, _)| m == id).map(|(_, i)| Control::RecallScene(*i))
        }
    }
}

type Built = (Menu, MenuId, MenuId, MenuId, Vec<(MenuId, usize)>);

fn build_menu(status: &MenuItem, scenes: &[String]) -> Result<Built, String> {
    let menu = Menu::new();
    let open = MenuItem::new("Open UR44 Mixer", true, None);
    let settings = MenuItem::new("Settings\u{2026}", true, None);
    let quit = MenuItem::new("Quit UR44 Mixer", true, None);
    let scenes_menu = Submenu::new("Recall Scene", !scenes.is_empty());
    let mut scene_ids = Vec::new();
    for (i, name) in scenes.iter().enumerate() {
        let item = MenuItem::new(name, true, None);
        scene_ids.push((item.id().clone(), i));
        scenes_menu.append(&item).map_err(|e| e.to_string())?;
    }
    menu.append_items(&[
        status,
        &PredefinedMenuItem::separator(),
        &open,
        &scenes_menu,
        &settings,
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .map_err(|e| e.to_string())?;
    Ok((menu, open.id().clone(), settings.id().clone(), quit.id().clone(), scene_ids))
}

#[derive(Debug, Clone)]
pub enum Incoming {
    Menu(MenuId),
    Launcher(Control),
}

/// Menu clicks and launcher requests, delivered to the app as messages.
pub fn events() -> impl Stream<Item = Incoming> {
    iced::stream::channel(64, |out: mpsc::Sender<Incoming>| async move {
        let mut menu_out = out.clone();
        std::thread::spawn(move || {
            while let Ok(ev) = MenuEvent::receiver().recv() {
                if block_on(menu_out.send(Incoming::Menu(ev.id))).is_err() {
                    break;
                }
            }
        });
        if let Some(listener) = LISTENER.get() {
            let listener = listener.try_clone().expect("socket");
            let mut sock_out = out.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let mut line = String::new();
                    let _ = BufReader::new(stream).read_line(&mut line);
                    let c = match line.trim() {
                        "settings" => Control::ShowSettings,
                        "quit" => Control::Quit,
                        _ => Control::Show,
                    };
                    if block_on(sock_out.send(Incoming::Launcher(c))).is_err() {
                        break;
                    }
                }
            });
        }
        std::future::pending::<()>().await;
    })
}
