//! System app-theme following. Never modifies the user's personalization settings.
use anti_webui::prelude::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub fn system_dark() -> bool {
    #[cfg(windows)]
    {
        use winreg::{RegKey, enums::HKEY_CURRENT_USER};
        return RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize")
            .and_then(|key| key.get_value::<u32, _>("AppsUseLightTheme"))
            .map(|v| v == 0)
            .unwrap_or(false);
    }
    #[cfg(not(windows))]
    {
        false
    }
}
pub fn follow_system() {
    set_theme(if system_dark() {
        Theme::dark()
    } else {
        Theme::light()
    });
    let sender = ui_channel(|dark| set_theme(if dark { Theme::dark() } else { Theme::light() }));
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    on_cleanup(move || stop.store(true, Ordering::Relaxed));
    std::thread::spawn(move || {
        let mut previous = system_dark();
        while !thread_stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(750));
            let dark = system_dark();
            if dark != previous {
                previous = dark;
                sender.send(dark);
            }
        }
    });
}
