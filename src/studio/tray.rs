//! The studio's tray icon. It keeps the app (and the live runtime it holds) alive while the
//! window is hidden. The icon is drawn in code, so the repo carries no image files.

use std::sync::mpsc::{Receiver, channel};

use eframe::egui;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use super::model::{Lang, icon_rgba};

pub enum Action {
    Open,
    ToggleLive,
    Quit,
}

pub struct Tray {
    _icon: TrayIcon,
    live: CheckMenuItem,
    open_id: tray_icon::menu::MenuId,
    live_id: tray_icon::menu::MenuId,
    quit_id: tray_icon::menu::MenuId,
    menu_rx: Receiver<MenuEvent>,
    icon_rx: Receiver<TrayIconEvent>,
}

impl Tray {
    pub fn new(ctx: &egui::Context, lang: Lang, accent: [u8; 3]) -> Result<Self, String> {
        let open = MenuItem::new(lang.pick("Open noren studio", "Otwórz noren studio"), true, None);
        let live = CheckMenuItem::new(
            lang.pick("Hold profile live", "Trzymaj profil na żywo"),
            true,
            false,
            None,
        );
        let quit = MenuItem::new(lang.pick("Quit", "Zakończ"), true, None);
        let menu = Menu::new();
        menu.append_items(&[&open, &live, &PredefinedMenuItem::separator(), &quit])
            .map_err(|e| e.to_string())?;
        let icon = Icon::from_rgba(icon_rgba(accent), 32, 32).map_err(|e| e.to_string())?;
        let tray = TrayIconBuilder::new()
            .with_tooltip("noren studio")
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .map_err(|e| e.to_string())?;
        // Events arrive on the UI thread's message loop, maybe while the window is hidden:
        // forward them and wake eframe so `App::logic` sees them.
        let (menu_tx, menu_rx) = channel();
        let c = ctx.clone();
        MenuEvent::set_event_handler(Some(move |e| {
            let _ = menu_tx.send(e);
            c.request_repaint();
        }));
        let (icon_tx, icon_rx) = channel();
        let c = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |e| {
            let _ = icon_tx.send(e);
            c.request_repaint();
        }));
        Ok(Self {
            _icon: tray,
            open_id: open.id().clone(),
            live_id: live.id().clone(),
            quit_id: quit.id().clone(),
            live,
            menu_rx,
            icon_rx,
        })
    }

    pub fn set_live(&self, on: bool) {
        self.live.set_checked(on);
    }

    pub fn actions(&self) -> Vec<Action> {
        let mut out = Vec::new();
        while let Ok(e) = self.menu_rx.try_recv() {
            let id = e.id();
            if *id == self.open_id {
                out.push(Action::Open);
            } else if *id == self.live_id {
                out.push(Action::ToggleLive);
            } else if *id == self.quit_id {
                out.push(Action::Quit);
            }
        }
        while let Ok(e) = self.icon_rx.try_recv() {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = e
            {
                out.push(Action::Open);
            }
        }
        out
    }
}
