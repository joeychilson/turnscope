# The macOS app

Turnscope.app is a menu bar item and the panel it opens. It shows what
`turnscope serve` sends and decides nothing itself. What's left of each
limit, where it's headed, which limit decides an account, the order of the
accounts and which alerts to raise all come from the engine
([protocol.md](protocol.md)). The app chooses the words, colors and layout.

It needs macOS 26 and is built with Xcode 26 and Swift 6
([development.md](development.md)). It has no Dock icon (`LSUIElement`).

## Layout

```
macos/
  Turnscope.xcodeproj   the app's one target, its build phases and packages
  Info.plist            bundle keys, and Sparkle's feed and public key
  Turnscope/            the app target: the AppKit shell, SwiftUI views, notifications, updates
  TurnscopeKit/         a Swift package: the contract, the client, the store, the words, and tests
  Logos/                provider and agent logos, copied into the app
```

`TurnscopeKit` holds everything that can be tested without a screen, and
`swift test` runs it on its own. `Turnscope/` holds what needs AppKit,
SwiftUI, UserNotifications, ServiceManagement or Sparkle. It has no tests.
Check it by running the app, or with `--fixture` and `--snapshot`.

`TurnscopeKit/Sources/TurnscopeKit/`:

| File | Holds |
|---|---|
| `Contract.swift` | The protocol's types (`Status`, `AccountStatus`, `LimitStatus`, `Outlook`, `Alert`, `UsedMost`, each request's parameters), the JSON decoder and encoder, and `Contract.status(from:)` for fixtures |
| `Client.swift` | The connection to `serve`: finding the socket, starting `serve`, `hello`, pushes, requests, reconnecting, and replacing another engine's `serve` |
| `Store.swift` | The observable state views read, and the requests they make through it |
| `Standing.swift` | Small facts read off the status: a limit's `standing` (lasts, running out, used up), an account's `decidingLimit`, `refused`, `refilledAt`, `twinned` |
| `Words.swift` | Figures, dollars and times |
| `Words+Limits.swift` | Limits, rows, the headline, the menu bar's figures, the panel's folds, and whether an agent is connected |
| `Words+Notes.swift` | Notifications |
| `Launch.swift` | Launch arguments: `Launch`, `Opening`, `SettingsTab` |

`Turnscope/`:

| File | Holds |
|---|---|
| `App.swift` | `main` and `AppDelegate`: the status item, the panel's window, and the wiring between the store, the item, `Notifier` and `Updater`. Picks the engine binary |
| `MenuBarItem.swift` | Draws the menu bar item |
| `Navigation.swift` | Which page and tab the panel shows, and what's open on it |
| `Notifier.swift` | Posts alerts as macOS notifications |
| `Updater.swift` | Updates with Sparkle |
| `Preferences.swift` | The app's own preference keys |
| `Logo.swift` | Draws a provider's or agent's logo |
| `Style.swift` | Shared sizes, fonts, colors and button styles |
| `Snapshot.swift` | `--snapshot`: draws the panel as PNGs |
| `Panel/Window.swift` | The panel's shell: its glass, edge and shadow, and the pages it slides between |
| `Panel/FirstPage.swift` | The panel's first page: the headline, the account lists, the offer to connect an agent, the footer |
| `Panel/AccountRow.swift` | An account's row, its limit lines and bars, and the sessions that used it most |
| `Panel/RowParts.swift` | Parts shared by rows: an account's title, the Hide and Connect buttons |
| `Panel/UnusedAccounts.swift` | The fold for accounts not used this week, and hidden accounts |
| `Settings/SettingsPage.swift` | The Settings page and its tabs |
| `Settings/GeneralTab.swift` | Menu bar, opening at login, notifications, updates |
| `Settings/AgentsTab.swift` | Connecting and disconnecting agents |
| `Settings/AccountsTab.swift` | Showing and hiding accounts |
| `Settings/SettingRow.swift` | The row and switch every setting uses |
| `Turnscope.icon` | The app icon, an Icon Composer file |

`TurnscopeKit/Tests/TurnscopeKitTests/`:

| File | Tests |
|---|---|
| `ContractTests.swift` | Every example in `contract/` decodes, and each request the app writes matches its example |
| `ClientTests.swift` | The client and store against a stand-in `serve` on a real socket |
| `EngineTests.swift` | The client and store against the real `turnscope serve`. Runs only when `TURNSCOPE_BINARY` is set |
| `LaunchTests.swift` | Launch arguments, and a store showing a fixture |
| `WordsTests.swift` | The wording, at a fixed time in UTC |
| `Support.swift` | What the tests share: the contract's files, and `FakeServe`, the stand-in |

## The Xcode project

`Turnscope.xcodeproj` has one target, Turnscope, built in this order:

1. **Sources**: the `Turnscope/` folder. New files in it are part of the
   target without editing the project.
2. **Frameworks**: `TurnscopeKit`, a local package, and Sparkle, pinned to
   2.10.0.
3. **Resources**: `Logos/`, copied as a folder to `Contents/Resources/Logos`.
4. **Build turnscope**: runs `scripts/build-cli.sh`, which builds the Rust
   binary for each architecture being built (release for a Release build,
   debug otherwise) and joins them into one file.
5. **Copy Helper**: copies `turnscope` to `Contents/Helpers` and signs it.

Swift warnings are errors. The bundle id is `com.joeychilson.turnscope`.
The project's version is a placeholder: the scripts set `MARKETING_VERSION`
and `CURRENT_PROJECT_VERSION` from `Cargo.toml`. `SPARKLE_PUBLIC_KEY` is
empty in the project, and `scripts/build-release.sh` sets it
([Updates](#updates)). The Turnscope scheme runs the app with
`--data /tmp/turnscope-dev`.

## How data flows

```
turnscope serve ──status, alerts──▶ Client ──events──▶ Store ──▶ views, menu bar item, Notifier
                ◀──── requests ────        ◀─ calls ──       ◀── clicks
```

- **`Client`** connects to `serve` and keeps connected. It runs
  `turnscope serve --socket` once for the socket's path, starts `serve` when
  nothing listens, and says `hello` on each connection. It hands the store
  events in order: its phase (connecting, connected, or failed and why),
  each `hello` result, each status and each alert. `request` sends a
  request and waits for its result. [protocol.md](protocol.md#lifecycle)
  says how it reconnects and replaces another engine's `serve`.
- **`Store`** is `@Observable`. It keeps the last status, even while
  `serve` is away, so the panel never goes empty during a restart.
  `failure` says why it isn't connected. It splits the accounts into the
  lists the panel shows (`inUse`, `recent`, `unused`, `hidden`), and makes
  the requests views ask for. Hiding an account and changing a notification
  setting show at once, and go back if `serve` refuses. Connecting or
  disconnecting an agent shows as soon as `serve` has done it. It also holds
  which agents are being connected, why connecting failed, and the sessions
  that used the open account's deciding limit (`usedMost`), shown only while
  that limit still decides it. It calls `onChange` after each status and
  each time `serve` connects or fails, and `onAlert` with each alert.
  `AppDelegate` sets these to redraw the menu bar item and post
  notifications.
- **Views** get the store from the SwiftUI environment, with `Navigation`,
  `Updater` and `PanelActions` (quit, close, and whether notifications are
  off).
- **`Words`** writes every figure and sentence made from the status: what's
  left, times, a limit's line, the headline, the menu bar's figures and
  notifications. Views get one from `store.words(now:)`, which uses the agent
  names in the status. Fixed labels, such as headings, button titles and
  setting names, are written in the view that shows them. Everything is in
  English, and times are on a 12-hour clock, whatever the Mac's language.
- **`Standing`** turns an outlook into lasts, running out or used up.
  `Style.swift` maps it to a color: gray, amber or red.

The app's state lives on the main thread: `Client`, `Store`, `Navigation`
and `AppDelegate` are `@MainActor`. Only the socket is read on a thread of
its own.

Countdowns move with the clock. The panel's words are as of
`Navigation.now`, which is set when the panel opens and each minute while
it's open. The menu bar item is redrawn each minute only while the panel
is open or the item shows an account running out or used up.

## The menu bar item

The item shows each account in use, most urgent first: its provider's logo,
then `Words.figure`:

- What's left, `68%`, in amber once it's running out.
- How long it has, `1h 40m`, within 3 hours of running out.
- When it's back, `Back 5:40 AM`, in red once it's used up.
- For an API key with no limit, what it cost this month, `$84`.

With nothing in use, it shows a gauge symbol. While every account it shows
lasts, it's a template image in the menu bar's own color. Once one needs
attention, it's drawn in color. While `serve` is away (`Store.failure`), it's
dimmed, since it shows the last status. With Settings → General → Show set to
Most urgent, it shows only the most urgent account. VoiceOver reads each
account's title and `Words.spokenFigure`, then "not up to date" while it's
dimmed.

The item is one image drawn by SwiftUI, redrawn only when what it shows
changes: a new status, `serve` going away or coming back, the menu bar
turning light or dark, a preference changing, or the minute's tick. Clicking
it opens or closes the panel.

## The panel

The panel is 340 points wide. From the top:

1. **The headline**: whether you can keep going, from the most urgent
   account in use (`Words.verdict`). Before any account is found, why none
   is (`Words.welcome`).
2. **Accounts in use**, most urgent first.
3. **Used this week**: accounts not in use but used in the last 7 days, the
   most left first.
4. **"3 more accounts"**: the rest, folded into one line. Inside it,
   "2 hidden" lists the hidden accounts, each with a Show button.
5. **An offer to connect an agent**, while an agent in use isn't connected
   or uses another copy of Turnscope: Connect (or Update), and a button
   that stops offering it for that agent.
6. **The footer**: "Not up to date · trying again" while `serve` is away,
   or "Reading your agents' history…" during the first read. Then "Update
   available" when a scheduled check found one ([Updates](#updates)),
   Settings (⌘,) and Quit (⌘Q).

Before the first status, the panel shows "Reading your agents' history…",
or "Turnscope can't read its data" with the reason.

An account's row shows its logo, its title (and its label when another
shown account has the same title), the line for its deciding limit, and
what's left of it on the right. A row with limits, or a note on what they
can't say (`Words.opened`), opens in place on a click, Space or Return,
closing any other. Open, it shows each limit on a line of its own with a
bar, the note, and the sessions that used the deciding limit most, from
`limit.breakdown`. The store asks for those of every account as the panel
opens, so a row opens with them in one motion, and again for the open
account with each status. Hovering shows a Hide button, and right-clicking
offers Hide too.

The window is a borderless, transparent panel hung under the item, as tall
as the screen allows. The SwiftUI view draws its own glass, edge and shadow
at the top of it, so its height can animate without the window resizing. A
click anywhere outside it, or switching apps, closes it. Escape closes it
from its first page.

The panel is as tall as what it shows. A Settings tab taller than the
screen allows scrolls under its title. The first page doesn't scroll: a
scroll view there is measured again on every frame of a row opening, which
took a whole core of the CPU against 5% without it (October 2026).

## Navigation

`Navigation` holds where the panel is:

| Property | Holds |
|---|---|
| `page` | `main` or `settings`. Settings slides in from the right, and Back or Escape returns |
| `tab` | The Settings tab shown |
| `open` | The id of the account open in place, if any |
| `unusedOpen` | Whether the accounts not used this week are unfolded |
| `hiddenShown` | Whether hidden accounts are listed |
| `now` | The time the panel's countdowns run from |

The panel always opens on its first page with nothing open
(`Navigation.reset`). `--open` and a clicked notification then set what's
open.

## Settings

Settings is the panel's second page, with three tabs. Changes take effect
at once.

| Tab | Holds |
|---|---|
| General | **Menu bar**: Show (All in use, or Most urgent) and Open at login. **Notify me when**: a switch for each kind of alert. **Updates**: the version, Check now and Check automatically |
| Agents | Each installed agent, with whether it's connected and one button: Connect, Update (it uses another copy of Turnscope) or Disconnect |
| Accounts | Subscriptions, then API keys, each with a switch to show or hide it |

Where each setting is kept:

| Setting | Kept in | Also changed by |
|---|---|---|
| Show (menu bar) | The app's defaults, `menuShows`: `all` or `one` | |
| Offers to connect put away | The app's defaults, `dismissedConnect`: agent ids joined by commas | |
| Open at login | macOS's login items (`SMAppService.mainApp`) | System Settings |
| Notify me when | The engine's settings, through `settings.set` | `turnscope config set notify.<kind> on\|off` |
| Hidden accounts | The engine, through `account.hide` | `turnscope accounts hide\|show` |
| Connected agents | Each agent's own configuration, through `agent.connect` and `agent.disconnect` | `turnscope connect`, `turnscope disconnect` |
| Check automatically | Sparkle's defaults | |

## Notifications

`Notifier` posts every alert `serve` sends. `serve` has already dropped the
kinds turned off and alerts for hidden accounts, so the app doesn't filter.

- **Permission.** It asks for permission when the app starts. If
  notifications are off for Turnscope in System Settings, Settings → General
  says so and offers to open them.
- **Words.** `Words.note` writes each one. The title is the account, the
  subtitle is what happened, and the body is when it's back or what to do.
  A title shared with another account gets part of the label. Times are
  clock times, since a notification stays on screen.
- **Replacing.** Each is posted under the alert's `replaces`, so a newer one
  for the same limit or account replaces the older: "back" replaces "used
  up". A sign-in notification is removed once the account can be read
  again.
- **Urgency.** Every notification is at the normal level: none breaks
  through Focus. Time-sensitive delivery needs an entitlement the app
  doesn't have, and a limit running out can wait until Focus ends.
- **Acknowledging.** The app acknowledges an alert (`alerts.ack`) only once
  macOS accepts the notification. One that macOS refused, such as when
  notifications are off, comes again with the next `hello`.
- **Clicking** a notification opens the panel with its account open.

## Updates

`Updater` uses Sparkle, pinned to 2.10.0 in the Xcode project. It reads the
appcast at `SUFeedURL`, the latest GitHub release's `appcast.xml`, checks
each update's signature with `SUPublicEDKey`, and asks before installing.
Automatic checks are on by default (`SUEnableAutomaticChecks`), and Settings
→ General → Check automatically turns them off.

The app has no window to bring forward, so an update found by a scheduled
check would open behind other apps. Instead it uses Sparkle's gentle
reminders: unless Sparkle can show the update in focus, as just after
launch, the panel's footer shows "Update available", which shows the
update. It goes once that update's session ends: installed, skipped or put
off.

`SUPublicEDKey` comes from the `SPARKLE_PUBLIC_KEY` build setting, which only
`scripts/build-release.sh` sets. A build without the key, such as one from
`scripts/build-app.sh` or Xcode, never checks for updates, and Settings
shows only its version. [development.md](development.md) says how releases
are signed.

## Opening at login

Settings → General → Open at login registers the app as a login item with
`SMAppService.mainApp`. If macOS holds it for approval, the switch stays on
and a row offers to open Login Items in System Settings. If registering
fails, the Mac beeps.

## Logos

`macos/Logos/providers/<provider id>.svg` and `agents/<agent id>.svg` are
drawn in one color (`currentColor`), so `Logo` can tint them like icons in
the color of the text beside them. An account uses its provider's logo,
and an agent its own. Without one, the app draws an SF Symbol: `key.fill`
for a provider, `terminal.fill` for an agent. Adding a logo needs no code:
add the file, named by the id. [macos/Logos/README.md](../macos/Logos/README.md)
says where each one comes from.

## Launch arguments

| Argument | Does |
|---|---|
| `--data <dir>` | Passed to the engine: the data directory to use |
| `--home <dir>` | Passed to the engine: the home folder to read agents' history and logins from |
| `--open` | Opens the panel once the first status arrives |
| `--open <names>` | Also opens what the names say, joined by commas: `--open account,unused` |
| `--fixture <file>` | Shows the status in a file, with no engine |
| `--snapshot <dir>` | Draws the panel as `panel-light.png` and `panel-dark.png` in the folder, then quits |

| `--open` name | Opens |
|---|---|
| `account` | The first account in use, in place |
| `unused` | The fold of accounts not used this week |
| `settings` | Settings, at General |
| `agents` | Settings, at Agents |
| `accounts` | Settings, at Accounts |

An unknown name opens nothing. A value that starts with `--` is never read
as an option's value, so `--open --data <dir>` opens nothing in the panel.

- **`--fixture`** reads a status on its own, a `status` notification or a
  `hello` result, such as `contract/status.notification.json`. Its words are
  as of the status's `at`, so it reads the same whenever it's drawn.
  Changes made in it show on screen and go nowhere.
- **`--snapshot`** needs `--fixture`. With `--open`, it opens a row or the
  fold first, or draws Settings at the tab named. It draws the window
  background where the panel has glass, the panel at its full height with
  nothing scrolled, and no sessions under an open account, since there's no
  engine to ask.

| Environment variable | Does |
|---|---|
| `TURNSCOPE_BINARY` | The `turnscope` to run instead of `Contents/Helpers/turnscope`. `EngineTests.swift` reads it too |

The engine the app starts inherits the app's environment, so
`TURNSCOPE_OFFLINE` and `TURNSCOPE_NOW` reach it. A `serve` already running
for that data directory is used as it is.

`open` starts the app in `/`, so give it absolute paths. Running the
executable directly keeps the shell's directory and environment:

```sh
open target/xcode/Build/Products/Debug/Turnscope.app --args --data /tmp/ts --open
app=target/xcode/Build/Products/Debug/Turnscope.app/Contents/MacOS/Turnscope
TURNSCOPE_BINARY=$PWD/target/debug/turnscope "$app" --data /tmp/ts --open account
"$app" --fixture contract/status.notification.json --open agents
```

The app logs to the unified log, under the subsystem
`com.joeychilson.turnscope`. A `serve` it starts writes nowhere. To watch
the connection:

```sh
log stream --predicate 'subsystem == "com.joeychilson.turnscope"'
```

## How to

### Build and run

```sh
scripts/build-app.sh   # target/xcode/Build/Products/Debug/Turnscope.app
```

Or open `macos/Turnscope.xcodeproj` in Xcode and run the Turnscope scheme.
Both build the Rust binary into the app. Use a scratch `--data`, never your
own data directory.

### Run the Swift tests

```sh
swift test --package-path macos/TurnscopeKit -Xswiftc -warnings-as-errors
```

To also run the client against the real engine:

```sh
cargo build
TURNSCOPE_BINARY="$PWD/target/debug/turnscope" swift test --package-path macos/TurnscopeKit
```

### Add a setting

First decide who owns it.

**An app preference** changes only how the app looks or behaves:

1. Add a key to `Preference` in `Preferences.swift`, with its default in
   `register()`.
2. Add a `SettingRow` for it in `GeneralTab.swift`, or the tab it belongs
   to, bound with `@AppStorage(Preference.<key>)`.
3. Read it where it applies. Code outside SwiftUI reads `UserDefaults`.
   `AppDelegate` redraws the menu bar item on any change to the defaults.

**An engine setting** must agree with the command line, or changes what
`serve` does, like the notification switches. It's a protocol change
([protocol.md](protocol.md#change-the-protocol)):

1. Add the field to `Settings` or `Notify` in `src/alerts.rs`, with its
   default, and to `turnscope config` in `src/main.rs` and
   [cli.md](cli.md).
2. Add it to the examples in `contract/` that carry settings.
3. Add it to `Settings` or `Notify` in `Contract.swift`. The app sends the
   whole `Settings` back, so a field it leaves out is reset to its default
   each time the app changes a setting.
4. Change it through a `Store` method like `setNotify`, and add its
   `SettingRow`.

### Add or change a string

- **A figure or sentence made from the status**, such as a limit's line, a
  time, the headline, the menu bar's figure or a notification: change it in
  `Words`, and its test in `WordsTests.swift`. `Words.swift` holds
  figures and times, `Words+Limits.swift` limits, rows, the headline, the
  menu bar, the panel's folds and agents' connections, and
  `Words+Notes.swift` notifications. Check what VoiceOver
  says too: `spokenFigure` for the menu bar, and `accessibilityLabel` in the
  views.
- **A fixed label**, such as a heading, a button or a setting's name: change
  it in the view that shows it.

Use the same words for the same thing everywhere: in the panel, the menu
bar and notifications, and in the command line and MCP server, which have
their own wording in `src/status/text.rs`. Redraw the screenshots if they
show the string.

### Show a new status field

The engine decides and the app only shows. If a view needs a threshold, an
order or a choice, make it in the engine and send the answer.

1. Add the field to the Rust type and to the examples in `contract/`
   ([protocol.md](protocol.md#change-the-protocol)). An added field doesn't
   need a new protocol version.
2. Add it to the type in `Contract.swift`.
3. Word it in `Words`, with a test in `WordsTests.swift`. If it sets a
   color, derive that in `Standing.swift`.
4. Show it in the view.
5. Look at it with `--fixture contract/status.notification.json`, and
   redraw the screenshots if they change.

### Redraw the screenshots

```sh
scripts/screenshots.sh
```

It builds the app and draws three pairs, light and dark, into
`.github/screenshots/` from `contract/status.notification.json`, which
holds only made-up accounts: `panel` (an account in use, open), `idle`
(nothing in use, from a copy of the example edited with `jq`) and `agents`
(Settings → Agents). The README shows them. To draw one by hand:

```sh
target/xcode/Build/Products/Debug/Turnscope.app/Contents/MacOS/Turnscope \
  --snapshot /tmp/shots --fixture contract/status.notification.json --open account
```
