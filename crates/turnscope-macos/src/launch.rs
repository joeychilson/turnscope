//! The app's launch arguments (docs/app.md#launch-arguments).

use std::path::PathBuf;

use crate::state::Tab;

#[derive(Debug, Default, PartialEq)]
pub struct Launch {
    pub fixture: Option<PathBuf>,
    pub snapshot: Option<PathBuf>,
    pub opening: Option<Opening>,
    pub data: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

impl Launch {
    pub fn parse(arguments: &[String]) -> Launch {
        let args = arguments.get(1..).unwrap_or(&[]);
        let value = |option: &str| {
            let index = args.iter().position(|a| a == option)?;
            args.get(index + 1)
                .map(String::as_str)
                .filter(|value| !value.starts_with("--"))
        };
        Launch {
            fixture: value("--fixture").map(PathBuf::from),
            snapshot: value("--snapshot").map(PathBuf::from),
            opening: args
                .iter()
                .any(|a| a == "--open")
                .then(|| Opening::parse(value("--open").unwrap_or_default())),
            data: value("--data").map(PathBuf::from),
            home: value("--home").map(PathBuf::from),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Opening {
    pub account: bool,
    pub unused: bool,
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
    fn parses_launch_options_and_open_targets() {
        assert_eq!(launch(""), Launch::default());
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
        let bare = launch("--fixture --open");
        assert_eq!(bare.fixture, None);
        assert_eq!(bare.opening, Some(Opening::default()));
        assert_eq!(Opening::parse("settings").settings, Some(Tab::General));
        assert_eq!(Opening::parse("agents").settings, Some(Tab::Agents));
        assert_eq!(Opening::parse("accounts").settings, Some(Tab::Accounts));
        assert_eq!(Opening::parse("nothing"), Opening::default());
    }
}
