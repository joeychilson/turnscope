//! The app's own preferences, kept in its defaults under the keys 0.2 used, so they
//! carry over. Which notifications are sent is the engine's setting instead.

use std::collections::HashSet;

use objc2_foundation::{NSString, NSUserDefaults};

/// What the menu bar shows, as a `MenuShows`: `all` or `one`.
const MENU_SHOWS: &str = "menuShows";
/// Agents in use for which the offer to connect them was put away, joined by commas.
const DISMISSED_CONNECT: &str = "dismissedConnect";

/// What the menu bar shows: every account in use, or only the most urgent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MenuShows {
    #[default]
    All,
    One,
}

#[derive(Clone, Debug, Default)]
pub struct Preferences {
    pub menu_shows: MenuShows,
    pub dismissed_connect: HashSet<String>,
    /// Whether they're read from and written to the app's defaults: not for a fixed status,
    /// whose changes go nowhere.
    kept: bool,
}

impl Preferences {
    /// The preferences in the app's defaults.
    pub fn read() -> Preferences {
        let defaults = NSUserDefaults::standardUserDefaults();
        let string = |key: &str| {
            defaults
                .stringForKey(&NSString::from_str(key))
                .map(|s| s.to_string())
        };
        Preferences {
            menu_shows: match string(MENU_SHOWS).as_deref() {
                Some("one") => MenuShows::One,
                _ => MenuShows::All,
            },
            dismissed_connect: string(DISMISSED_CONNECT)
                .map(|ids| {
                    ids.split(',')
                        .filter(|id| !id.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            kept: true,
        }
    }

    pub fn set_menu_shows(&mut self, shows: MenuShows) {
        self.menu_shows = shows;
        self.write(
            MENU_SHOWS,
            if shows == MenuShows::One {
                "one"
            } else {
                "all"
            },
        );
    }

    /// Stop offering to connect the agent `id`.
    pub fn dismiss_connect(&mut self, id: &str) {
        self.dismissed_connect.insert(id.to_owned());
        let mut ids: Vec<&str> = self.dismissed_connect.iter().map(String::as_str).collect();
        ids.sort();
        let joined = ids.join(",");
        self.write(DISMISSED_CONNECT, &joined);
    }

    fn write(&self, key: &str, value: &str) {
        if !self.kept {
            return;
        }
        let defaults = NSUserDefaults::standardUserDefaults();
        unsafe {
            defaults.setObject_forKey(Some(&NSString::from_str(value)), &NSString::from_str(key))
        };
    }
}
