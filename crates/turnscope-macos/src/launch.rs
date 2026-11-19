//! The app's launch arguments (docs/app.md#launch-arguments).

use std::path::PathBuf;

use crate::state::Tab;

#[derive(Debug, Default, PartialEq)]
pub struct Launch {
    /// The status to show instead of the engine's.
    pub fixture: Option<PathBuf>,
    /// Where to draw the panel before quitting.
    pub snapshot: Option<PathBuf>,
    /// What to open once the first status arrives, or None to open nothing.
    pub opening: Option<Opening>,
    /// The data folder, instead of the default.
    pub data: Option<PathBuf>,
    /// The home folder whose agents are read, instead of the person's.
    pub home: Option<PathBuf>,
}

impl Launch {
    /// The options in `arguments`, as the command line gives them, with the program first.
    pub fn parse(arguments: &[String]) -> Launch {
        // The value given after `option`, or None if it's last or another option follows it.
        let value = |option: &str| {
            let index = arguments.iter().position(|a| a == option)?;
            arguments
                .get(index + 1)
                .filter(|value| !value.starts_with("--"))
                .cloned()
        };
        Launch {
            fixture: value("--fixture").map(PathBuf::from),
            snapshot: value("--snapshot").map(PathBuf::from),
            opening: arguments
                .iter()
                .any(|a| a == "--open")
                .then(|| Opening::parse(&value("--open").unwrap_or_default())),
            data: value("--data").map(PathBuf::from),
            home: value("--home").map(PathBuf::from),
        }
    }
}

/// What `--open` opens in the panel: names joined by commas, such as `account,unused`. An
/// unknown name opens nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Opening {
    /// The first account in use, opened in place.
    pub account: bool,
    /// The accounts not used this week, unfolded.
    pub unused: bool,
    /// Settings, opened at this tab.
    pub settings: Option<Tab>,
}

impl Opening {
    pub fn parse(names: &str) -> Opening {
        let mut opening = Opening::default();
        for name in names.split(',') {
            match name {
                "account" => opening.account = true,
                "unused" => opening.unused = true,
                "settings" => opening.settings = Some(Tab::General),
                "agents" => opening.settings = Some(Tab::Agents),
                "accounts" => opening.settings = Some(Tab::Accounts),
                _ => {}
            }
        }
        opening
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(arguments: &str) -> Launch {
        let arguments: Vec<String> = std::iter::once("Turnscope")
            .chain(arguments.split_whitespace())
            .map(str::to_owned)
            .collect();
        Launch::parse(&arguments)
    }

    #[test]
    fn nothing_given_opens_nothing() {
        assert_eq!(launch(""), Launch::default());
    }

    #[test]
    fn each_option_takes_the_value_after_it() {
        let given =
            launch("--fixture f.json --snapshot out --data d --home h --open account,unused");
        assert_eq!(given.fixture, Some(PathBuf::from("f.json")));
        assert_eq!(given.snapshot, Some(PathBuf::from("out")));
        assert_eq!(given.data, Some(PathBuf::from("d")));
        assert_eq!(given.home, Some(PathBuf::from("h")));
        assert_eq!(
            given.opening,
            Some(Opening {
                account: true,
                unused: true,
                settings: None
            })
        );
    }

    #[test]
    fn an_option_followed_by_another_has_no_value() {
        let given = launch("--fixture --open");
        assert_eq!(given.fixture, None);
        assert_eq!(given.opening, Some(Opening::default()));
    }

    #[test]
    fn open_names_a_settings_tab() {
        assert_eq!(Opening::parse("settings").settings, Some(Tab::General));
        assert_eq!(Opening::parse("agents").settings, Some(Tab::Agents));
        assert_eq!(Opening::parse("accounts").settings, Some(Tab::Accounts));
        assert_eq!(Opening::parse("nothing"), Opening::default());
    }
}
