# The macOS app

Turnscope.app is a menu bar item and the panel it opens. It runs the
engine in its own process, on threads of its own: the engine keeps the data
current and decides everything shown. What's left of each limit, where it's
headed, which limit decides an account, the order of the accounts and which
alerts to raise all come from the engine. The app chooses the words, colors
and layout.

It needs macOS 26. It's written in Rust, like the engine, and built with
Cargo ([development.md](development.md)). The panel is drawn with
[Rune](https://github.com/anomalyco/rune). The menu bar item,
notifications, opening at login and updates use AppKit and its frameworks,
called through `objc2`. It has no Dock icon (`LSUIElement`).

## Layout

```
crates/turnscope-macos/
  Cargo.toml          the app's crate; its binary is turnscope-app
  build.rs            builds the logos into the app
  assets/
    Info.plist        bundle keys, and Sparkle's feed; scripts/app.sh adds the version and key
    Turnscope.icon    the app icon, an Icon Composer file
    logos/            provider and agent logos
  src/                the app
```

`src/`:

| File | Holds |
|---|---|
| `main.rs` | `main`: the launch arguments, the panel's window, the minute's tick, and what to do with each status the engine gives (`took`) |
| `engine.rs` | Passes what the engine tells on to `State`, and the app's requests on to the engine |
| `state.rs` | The app's state, which every view reads: the last status, where the panel is, and the lists of accounts it shows |
| `preferences.rs` | The app's own preferences |
| `standing.rs` | The status's types, with small facts read off them: a limit's standing (lasts, running out, used up), an account's `refilled_at` and `twinned` |
| `words.rs` | Every figure and sentence made from the status, and its tests |
| `launch.rs` | Launch arguments: `Launch` and `Opening` |
| `menu_bar.rs` | Draws the menu bar item, and places the panel under it |
| `panel.rs` | The panel: its pages, the headline, the account rows, the folds, the offer to connect an agent, the footer |
| `settings.rs` | The Settings page and its tabs |
| `style.rs` | Shared sizes, fonts, colors and controls: buttons, pills, switches, the spinner |
| `symbols.rs` | Draws SF Symbols, which Rune can't |
| `notifier.rs` | Posts alerts as macOS notifications |
| `updater.rs` | Updates with Sparkle |
| `login.rs` | Opening at login |

The tests run with the engine's, `cargo test --workspace`. Those of
`words.rs` check the wording at a fixed time in UTC. Those of `state.rs`
check what the panel lists, from the example status in
`crates/turnscope/tests/fixtures/status.json`. Those of `launch.rs` check the launch
arguments. What needs a screen is checked by running the app, or with
`--fixture` and `--snapshot`.

## The bundle

`scripts/app.sh` makes `Turnscope.app` from two binaries:

```
Turnscope.app/Contents/
  Info.plist
  MacOS/Turnscope          the app, built as turnscope-app
  Helpers/turnscope        the command line, which connected agents run
  Frameworks/Sparkle.framework
  Resources/               the icon, compiled from assets/Turnscope.icon
```

The app's binary is built as `turnscope-app`, since a folder can't hold
both `Turnscope` and `turnscope` on a Mac's disk. The bundle id is
`com.joeychilson.turnscope`. The version is the crate's, from
`Cargo.toml`. Sparkle is fetched by `scripts/sparkle.sh`, at the version
it names, checked against its checksum.

## How data flows

```
turnscope::engine ──events──▶ engine.rs ──▶ State ──▶ panel, menu bar item, notifier
                  ◀─requests─           ◀── clicks
```

The engine is the same for every app: an app starts
`turnscope::engine::Engine` and shows each `Event` it tells.

- **The engine's thread** opens the data, takes `app.lock` in it, and
  keeps it current: it reads the agents' history and logins as they
  change, reads the limits, and tells the app each status that shows
  something new, with the alerts it raised. If the data can't be opened or kept
  current, it says why (`State::failure`) and tries again every 5 s.
  While another app holds the lock, as a second copy of Turnscope does, it
  shows what that one writes. Once a newer Turnscope has written to the
  data, it stops, and says to update. What goes wrong is written to
  `app.log` in the data directory.
- **A request**, such as hiding an account, changing a notification
  setting, acknowledging an alert, connecting an agent or asking what used a
  limit, runs on a thread of its own. Then the status is told again.
  Hiding an account and changing a setting show at once, and go back if the
  change failed. Connecting or disconnecting an agent shows once it's done.
- **`took`**, on the main thread, keeps the status (`State::take`), opens
  what waited for the first status, redraws the menu bar item, and posts
  the alerts. While the panel is open, the status's changes move, and it
  asks what used the accounts that need it ([The panel](#the-panel)). The
  status is kept until a newer one arrives, even while the data can't be
  kept current, so the panel never goes empty.
- **`State`** splits the accounts into the lists the panel shows (`in_use`,
  `recent`, `unused`, `hidden`). It holds which agents are being
  connected, why connecting failed, and the sessions that used an account's
  deciding limit (`used`), shown only while that limit still decides it.
- **Run from its disk image**, or from the copy macOS runs it from there
  (App Translocation), the app offers to connect no agent
  (`State::from_disk_image`), and Settings → Agents turns off its switches
  and says to move Turnscope to Applications. An agent connected there would
  run a `turnscope` that's gone once the image is ejected.
- **`Words`** writes every figure and sentence made from the status: what's
  left, times, a limit's line, the headline, the menu bar's figures,
  notifications and what VoiceOver says. Views get one from `State::words`,
  which uses the agents' names, gathered as the status is taken. A page
  makes one each time it's drawn, and hands it to what it shows. Fixed
  labels, such as headings, button titles and setting names, are written in
  the view that shows them.
  Everything is in English, dates are on the Gregorian calendar, and times
  are on a 12-hour clock, in the Mac's time zone.
- **A standing** is lasts, running out or used up. `style.rs` maps it to a
  color: gray, amber or red. Amber and red have a variant for light, dark
  and Increase Contrast, each at 4.5:1 or more against an open row (7:1 with
  Increase Contrast). With Increase Contrast, the grays, the panel's edge,
  an open row's fill and the bars' track are the system's stronger ones
  too (`style::Ink`).

The state lives on the main thread. The engine and the requests change it
through Rune's `AppHandle::update`, which runs on the main thread.

Countdowns move with the clock. The panel's words are as of `State::now`,
which is set when the panel opens and each minute while it's open. The
menu bar item is redrawn each minute only while the panel is open or the
item shows an account running out or used up; otherwise the app doesn't
wake. It's also redrawn as the Mac wakes from sleep, which the minute's
tick sleeps through. `Words` counts from the minute it is, as the engine
does, so at 12:00:40 a reset at 14:00 is in 2h.

## The menu bar item

The item shows each account in use, most urgent first: its provider's logo,
then `Words::figure`:

- What's left, `68%`, in amber once it's running out.
- How long it has, `1h 40m`, within 3 hours of running out.
- When it's back, `Back 5:40 AM`, in red once it's used up.
- For an API key with no limit, what it cost this month, `$84`.

With nothing in use, it shows Turnscope's logo, the ring and slice of its
icon. While every account it shows lasts, it's a template image in the menu
bar's own color. Once one needs attention, it's drawn in color, resolved
in the menu bar's own appearance as it's drawn. While the data can't be
kept current, it's dimmed, since it shows the last status. With Settings →
General → Show set to Most urgent, it shows only the most urgent account.
VoiceOver reads `Words::spoken_item`: each account's title and what its
figure says, then "not up to date" while it's dimmed.

The item is one image, drawn again only when what it shows changes. The
image draws itself each time AppKit asks, so it follows the menu bar
turning light or dark. Clicking it opens or closes the panel.

## The panel

The panel is 340 points wide. It hangs at the top of its window, which is
see-through, runs to the bottom of the screen, and is 28 points wider on
each side for the panel's shadow. It's placed in points, which span every
screen whatever its scale (`menu_bar::panel_place`). The panel grows and
shrinks inside it, so the window never resizes while it moves. A click
beside or below the panel lands on that window, so it closes the panel, as
a click outside a menu does, and goes no further. Rune lets clicks through
only its overlays. From
the top:

1. **The headline**: whether you can keep going, from the most urgent
   account in use (`Words::verdict`). It names the account by its title
   alone: it's about the first account listed under it, whose row shows its
   label. Before any account is found, why none is (`Words::welcome`).
2. **Accounts in use**, most urgent first.
3. **Used this week**: accounts not in use but used in the last 7 days, the
   most left first.
4. **"3 more accounts"**: the rest, folded into one line. Inside it,
   "2 hidden" lists the hidden accounts, each with a Show button.
5. **An offer to connect an agent**, while an agent in use isn't connected
   or uses another copy of Turnscope. It's laid out as an account's row: the
   agent's name, and under it what's wrong. Beside them, Connect (or
   Reconnect), and a round button as tall that stops offering it for that
   agent. It stands apart from the accounts, as a group of its own.
6. **The footer**: "Not up to date · trying again" while the data can't be
   kept current, "Not up to date · update Turnscope" once a newer Turnscope
   has written to it ("· start Turnscope again" when it was this one,
   updated while it ran), or "Reading your agents' history…" during the
   first read. Then "Update available" when a scheduled check found one
   ([Updates](#updates)), Settings (⌘,) and Quit (⌘Q). The words are cut
   short before the buttons are.

Before the first status, the panel shows "Reading your agents' history…",
or "Turnscope can't read its data" with the reason. Settings → Agents and
Accounts show the same.

On a first launch, the headline says "Getting your limits" until the
engine has read them. It gives a status then, before it reads the history,
so the accounts show while the footer says "Reading your agents'
history…". Until that read ends (`history_read` false), which accounts are
in use isn't known, so every account shown counts as in use
(`State::counts`): the headline is about the most urgent, the menu bar
shows each, and none is folded away. The engine lists them as accounts in
use until then.

An account's row shows its logo, its title (and its label when another
shown account has the same title), the line for its deciding limit, and
what's left of it on the right. A row with limits, or a note on what they
can't say (`Words::opened`), opens in place on a click, Space or Return,
closing any other. Open, it shows each limit on a line of its own with a
bar, the note, and the sessions that used the deciding limit most. The app
asks for those of every account as the panel opens, so a row opens with
them in one motion. With each status while it's open, it asks again for the
open account, and for each account shown that has none, such as one that
appeared since.
Hovering shows a Hide button, and right-clicking offers Hide too. Hide and
Show name the account as its row does (`Words::called`), so two with the
same title are told apart. Once the engine has stopped for good, as after a
newer Turnscope wrote the data, Hide and Show are off. A login that expired
says to use its agent to update, or that the agent hasn't refreshed its
saved login while it's in use. A limit that reset since it was read and was
used here since shows `—` instead of `100%`.

Tab comes to the rows once, and Up and Down move between them and the
folds. Return or Space opens a row. With Full Keyboard Access on, Tab goes
on to each row's Hide button, which shows while the keyboard is on it, and
to every other button. VoiceOver reads a row's header as one button: its
title, line and figure, and whether it's open. It reads a limit's figure as
what's left: "87% left".

Rune draws hover itself, without running the view again. A row's fill
fades in and out over 0.12 s, and a closed row darkens while it's pressed,
as the buttons do. A row opens or closes in one color: its fill changes at
once while its height springs. While the panel is open, a new status fades a figure or the
headline to its new words, and slides a row moving between accounts in use
and used this week to its new place. A row's bars grow as its lines fade
in.

With Reduce Motion on, Rune moves nothing: pages switch and bars are drawn
at their level, with a short fade, and the spinner holds still.

The window is a borderless panel hung under the item, with the system's
glass behind it. It's as tall as what it shows: a Settings tab taller than
the screen allows scrolls under its title. A click anywhere outside it, or
switching apps, closes it. Escape closes it from its first page, and goes
back from Settings.

## Where the panel is

`State` holds where the panel is:

| Field | Holds |
|---|---|
| `page` | `Main` or `Settings`. Settings slides in from the right, and Back or Escape returns |
| `tab` | The Settings tab shown |
| `open` | The id of the account open in place, if any |
| `unused_open` | Whether the accounts not used this week are unfolded |
| `hidden_shown` | Whether hidden accounts are listed |
| `now` | The time the panel's countdowns run from |

The panel always opens on its first page with nothing open
(`State::reset`). `--open` and a clicked notification then set what's
open.

## Settings

Settings is the panel's second page, with three tabs. Each tab's settings
are in cards. Changes take effect at once.

| Tab | Holds |
|---|---|
| General | **Menu bar**: Show (All in use, or Most urgent) and Open at login. **Notify me when**: a switch for each kind of alert, grayed out until the first status arrives, and once the engine has stopped for good. **Updates**: the version, Check now and Check automatically |
| Agents | Each installed agent, with a switch that connects or disconnects it. Under its name: that it uses another copy of Turnscope, which turning it on fixes, or why connecting failed. Run from its disk image, the switches are off and the heading says to move Turnscope to Applications. With no agent installed, what the panel's headline says then (`Words::welcome`): "No agents found", and the agents Turnscope reads |
| Accounts | Subscriptions, then API keys, each with a switch to show or hide it, off once the engine has stopped for good. With no account, what the panel's headline says (`State::verdict`), such as "Getting your limits" |

Left and Right move between the tabs, and between the options of Show. A
tab that scrolls fades at the edge with more under it.

Where each setting is kept:

| Setting | Kept in | Also changed by |
|---|---|---|
| Show (menu bar) | The app's defaults, `menuShows`: `all` or `one` | |
| Offers to connect put away | The app's defaults, `dismissedConnect`: agent ids joined by commas | |
| Open at login | macOS's login items (`SMAppService.mainApp`) | System Settings |
| Notify me when | The engine's settings, in the database | |
| Hidden accounts | The database | `turnscope accounts hide\|show` |
| Connected agents | Each agent's own configuration | `turnscope connect`, `turnscope disconnect` |
| Check automatically | Sparkle's defaults | |

## Notifications

`notifier.rs` posts every alert the engine raises. The engine has already
dropped the kinds turned off and alerts for hidden accounts, so the app
doesn't filter. macOS posts notifications only for an app bundle, so the
app run with `cargo run` posts none.

- **Permission.** It asks for permission when the app starts. If
  notifications are off for Turnscope in System Settings, Settings → General
  says so and offers to open them.
- **Words.** `Words::note` writes each one. The title is the account, the
  subtitle is what happened, and the body is when it's back or what to do.
  The title is the account's, with its label only when another account in
  use has the same title (`Words::called_among`). Times are clock times,
  since a notification stays on screen.
- **Replacing.** Each is posted under the alert's `replaces`, so a newer one
  for the same limit or account replaces the older: "back" replaces "used
  up". A sign-in notification is removed once the account can be read
  again, also one left from before the app started.
- **Urgency.** Every notification is at the normal level: none breaks
  through Focus. Time-sensitive delivery needs an entitlement the app
  doesn't have, and a limit running out can wait until Focus ends.
- **Acknowledging.** The app acknowledges an alert only once macOS accepts
  the notification. One that macOS refused, such as when notifications are
  off, comes again when the app next starts, posted with the first status,
  whose accounts and agents its words name.
- **Clicking** a notification opens the panel with its account open.
  Clicked before the first status, such as by the click that launches the
  app, the panel opens on what it shows until then, and the account opens
  once the status arrives, unless the panel was closed or went to Settings
  by then. Just after launch, the panel waits for the item to have a place
  in the menu bar, until 2 seconds after launch; with none by then, as when
  the item is hidden in the menu bar's settings, it opens at the screen's
  top right.

## Updates

`updater.rs` uses Sparkle. It reads the appcast at `SUFeedURL`, the latest
GitHub release's `appcast.xml`, checks each update's signature with
`SUPublicEDKey`, and asks before installing.
`SUVerifyUpdateBeforeExtraction` has it check the disk image's signature
before it mounts the image, rather than after, so a disk image that isn't
ours is never opened. Automatic checks are on by default
(`SUEnableAutomaticChecks`), and Settings → General → Check automatically
turns them off.

The app has no window to bring forward, so an update found by a scheduled
check would open behind other apps. Instead it uses Sparkle's gentle
reminders: unless Sparkle can show the update in focus, as just after
launch, the panel's footer shows "Update available", which shows the
update. It goes once that update's session ends: installed, skipped or put
off.

Sparkle is loaded from the bundle when the app starts, not linked, so the
app also runs with `cargo run`. It's loaded only when `SUPublicEDKey` is
set, which `scripts/app.sh` does from `SPARKLE_PUBLIC_KEY`, as only the
release workflow gives it. A build without the key never checks for
updates, and Settings
shows only its version. [development.md](development.md) says how releases
are signed.

## Opening at login

Settings → General → Open at login registers the app as a login item with
`SMAppService.mainApp`. If macOS holds it for approval, the switch stays on
and a row offers to open Login Items in System Settings. If registering
fails, the Mac beeps.

## Logos

`build.rs` builds every SVG in `assets/logos/` into the app. The panel
tints the one-color logos like text, and the menu bar item like its
figures. Where there's none, an SF Symbol stands in
([assets/logos/README.md](../crates/turnscope-macos/assets/logos/README.md)).

## Launch arguments

| Argument | Does |
|---|---|
| `--data <dir>` | The data directory to use |
| `--home <dir>` | The home folder to read agents' history and logins from, and to connect agents in |
| `--open` | Opens the panel once the first status arrives |
| `--open <names>` | Also opens what the names say, joined by commas: `--open account,unused` |
| `--fixture <file>` | Shows the status in a file, and runs no engine |
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

- **`--fixture`** reads a status as
  `crates/turnscope/tests/fixtures/status.json` has it:
  the status with its agents. Its words are as of the status's `at`, so it
  reads the same whenever it's drawn. Changes made in it show on screen and
  go nowhere: nothing is written to the app's defaults, the login items or
  Sparkle's settings, and neither Sparkle nor notifications start. To see
  a first launch while the history is read, with
  no account known to be in use:

  ```sh
  jq '.historyRead = false
    | .accounts |= map(.inUse = false | .recent = false | .usedBy = [])' \
    crates/turnscope/tests/fixtures/status.json > /tmp/first-launch.json
  cargo run -p turnscope-macos -- --fixture /tmp/first-launch.json --open
  ```
- **`--snapshot`** draws the status in `--fixture`, or else the status the
  engine works out from `--data` and `--home` as they are, reading only:
  it makes no database, raises no alert and catches nothing up. With `--open`, it opens a row
  or the fold first, or draws Settings at the tab named. It draws the
  window background where the panel has glass, the panel at its full height
  with nothing scrolled, and no sessions under an open account.

The engine runs in the app, so `TURNSCOPE_OFFLINE` and `TURNSCOPE_NOW`
apply to it as to the command line ([cli.md](cli.md#environment)).

`open` starts the app in `/`, so give it absolute paths. Running the
binary directly keeps the shell's directory and environment:

```sh
open target/debug/Turnscope.app --args --data /tmp/ts --open
cargo run -p turnscope-macos -- --data /tmp/ts --open account
cargo run -p turnscope-macos -- --fixture crates/turnscope/tests/fixtures/status.json --open agents
```

The app writes why the data can't be kept current, and why it stopped, to
`app.log` in the data directory. `turnscope doctor` shows what it logged
in the last day.

## How to

[development.md](development.md) says how to build and run the app, and
run its tests.

### Add a setting

First decide who owns it.

**An app preference** changes only how the app looks or behaves:

1. Add a key and a field to `Preferences` in `preferences.rs`, with its
   default, read in `Preferences::read` and written by a setter.
2. Add a setting for it to a group in `settings.rs`, in the tab it belongs
   to.
3. Read it where it applies. If it changes the menu bar item, redraw the
   item with `menu_bar::draw` once it's set.

**An engine setting** changes what the engine does, like the notification
switches:

1. Add the field to `Settings` or `Notify` in
   `crates/turnscope/src/alerts.rs`, with its
   default.
2. Change it in `settings.rs` as the notification switches do: change
   `status.settings` and call `engine::save_settings`.

### Add or change a string

- **A figure or sentence made from the status**, such as a limit's line, a
  time, the headline, the menu bar's figure or a notification: change it in
  `Words`, and its test. Check what VoiceOver says too: `spoken_item` for
  the menu bar, and each element's `label` in the views.
- **A fixed label**, such as a heading, a button or a setting's name: change
  it in the view that shows it.

Use the same words for the same thing everywhere: in the panel, the menu
bar and notifications, and in the command line and MCP server, which have
their own wording in `crates/turnscope/src/status/text.rs`. Redraw the screenshots if they
show the string.

### Show a new status field

The engine decides and the app only shows. If a view needs a threshold, an
order or a choice, make it in the engine and give the answer.

1. Add the field to its type in `crates/turnscope/src/status.rs`, and to
   `crates/turnscope/tests/fixtures/status.json`.
2. Word it in `Words`, with a test. If it sets a color, derive that in
   `standing.rs`.
3. Show it in the view.
4. Look at it with `--fixture`, and redraw the
   screenshots if they change.

### Redraw the screenshots

`scripts/screenshots.sh` builds the app and draws the README's
screenshots, light and dark, from
`crates/turnscope/tests/fixtures/status.json`.
