# The macOS app

Turnscope.app is a menu bar item and the panel it opens. It runs the
engine in its own process, on threads of its own: the engine keeps the data
current and decides everything shown. What's left of each limit, where it's
headed, which limit decides an account, the order of the accounts and which
alerts to raise all come from the engine. The app chooses the words, colors
and layout. A view that needs a threshold, an order or a choice gets it
from the engine.

It needs macOS 26. It's written in Rust, like the engine, in
`crates/turnscope-macos`. The panel is drawn with
[Rune](https://github.com/anomalyco/rune). The menu bar item,
notifications, opening at login and updates use AppKit and its frameworks,
called through `objc2`. It has no Dock icon (`LSUIElement`).
[development.md](development.md) says how to build, run and test it.

## The bundle

`scripts/app.sh` makes `Turnscope.app`:

```
Turnscope.app/Contents/
  Info.plist
  MacOS/Turnscope          the app, built as turnscope-app
  Helpers/turnscope        the command line, which connected agents run
  Frameworks/Sparkle.framework
  Resources/               the icon, compiled from assets/Turnscope.icon
```

The app's binary is built as `turnscope-app` since a folder on a Mac's
disk can't hold both `Turnscope` and `turnscope`.

## How data flows

```
turnscope::engine ──events──▶ engine.rs ──▶ State ──▶ panel, menu bar item, notifier
                  ◀─requests─           ◀── clicks
```

The app starts `turnscope::engine::Engine` and shows each `Event` it tells.
[architecture.md](architecture.md#the-apps-engine) says what the engine
does.

- **A request**, such as hiding an account or changing a setting, runs on
  a thread of the engine's. Hiding an account and changing a setting show
  at once, and go back if the change failed. Connecting or disconnecting
  an agent shows once it's done.
- **`took`**, on the main thread, keeps the status (`State::take`), opens
  what waited for the first status, redraws the menu bar item, and posts
  the alerts. The status is kept until a newer one arrives, even while the
  data can't be kept current, so the panel never goes empty.
- **`State`** splits the accounts into the lists the panel shows (`in_use`,
  `recent`, `unused`, `hidden`), and holds where the panel is, which agents
  are being connected, and the sessions that used each account's deciding
  limit. It lives on the main thread; the engine's threads change it
  through Rune's `AppHandle::update`.
- **`words.rs`** writes every figure and sentence made from the status:
  what's left, times, a limit's line, the headline, the menu bar's
  figures, notifications and what VoiceOver says. Fixed labels, such as
  headings, button titles and setting names, are written in the view that
  shows them. Everything is in English, on the Gregorian calendar and a
  12-hour clock, in the Mac's time zone.
- **`style.rs`** colors a [standing](architecture.md#glossary) gray,
  amber or red. Amber and red are 4.5:1 or more against an open row in
  light and dark, and 7:1 with Increase Contrast.
- **Run from its disk image**, or from the copy macOS runs it from there
  (App Translocation), the app offers to connect no agent, and Settings →
  Agents turns off its switches and says to move Turnscope to
  Applications. An agent connected there would run a `turnscope` that's
  gone once the image is ejected.

Countdowns move with the clock. The panel's words are as of `State::now`,
set when the panel opens and each minute while it's open. The menu bar
item is redrawn each minute only while the panel is open or the item shows
an account running out or used up, and as the Mac wakes; otherwise the app
doesn't wake. The app counts from the minute it is, as the engine does, so
at 12:00:40 a reset at 14:00 is in 2h.

## The menu bar item

The item shows each account in use, most urgent first: its provider's logo,
then its figure:

- What's left, `68%`, in amber once it's running out.
- How long it has, `1h 40m`, within 3 hours of running out.
- When it's back, `Back 5:40 AM`, in red once it's used up.
- For an API key with no limit, what it cost this month, `$84`.

With nothing in use, it shows Turnscope's logo. While every account it
shows lasts, it's a template image in the menu bar's own color; once one
needs attention, it's drawn in color. While the data can't be kept
current, it's dimmed, since it shows the last status. With Settings →
General → Show set to Most urgent, it shows only the most urgent account.
VoiceOver reads each account's title and what its figure says, then "not
up to date" while it's dimmed. Clicking the item opens or closes the
panel.

## The panel

The panel is 340 points wide. From the top:

1. **The headline**: whether you can keep going, from the most urgent
   account in use. It names the account by its title alone, as the first
   row under it shows the label. Before any account is found, why none is.
2. **Accounts in use**, most urgent first.
3. **Used this week**: accounts not in use but used in the last 7 days,
   the most left first.
4. **"3 more accounts"**: the rest, folded into one line. Inside it,
   "2 hidden" lists the hidden accounts, each with a Show button.
5. **An offer to connect an agent**, while an agent in use isn't connected
   or runs another copy of Turnscope: Connect (or Reconnect), and a button
   that stops offering it for that agent.
6. **The footer**: "Not up to date" and why while the data can't be kept
   current, or "Reading your agents' history…" during the first read. Then
   "Update available" when a scheduled check found one
   ([Updates](#updates)), Settings (⌘,) and Quit (⌘Q).

On a first launch, the headline says "Getting your limits" until the
engine has read them. The accounts show then, while the footer says the
history is being read. Until that read ends, which accounts are in use
isn't known, so every account shown counts as in use: the headline is
about the most urgent, the menu bar shows each, and none is folded away.

An account's row shows its logo, its title (and its label when another
shown account has the same title), the line for its deciding limit, and
what's left of it. A row opens in place on a click, Space or Return,
closing any other. Open, it shows each limit on a line of its own with a
bar, a note on what the limits can't say, and the sessions that used the
deciding limit most. The app asks for those as the panel opens, so a row
opens with them in one motion. Hovering a row shows a Hide button, and
right-clicking offers Hide too. An account whose login expired goes on
showing what was last read, as its agent renews the login when it runs.

Tab comes to the rows once, and Up and Down move between them and the
folds. With Full Keyboard Access on, Tab goes on to each row's Hide button
and every other button. VoiceOver reads a row's header as one button: its
title, line and figure, and whether it's open.

While the panel is open, a new status fades a changed figure or headline
to its new words, and slides a row that moves between lists. With Reduce
Motion on, nothing moves: pages switch and bars are drawn at their level,
with a short fade.

The panel is a borderless window hung under the item, with the system's
glass behind it. The window runs to the bottom of the screen, so the panel
grows and shrinks inside it without the window resizing, and a Settings
tab taller than the screen scrolls under its title. A click beside or
below the panel lands on that window and closes the panel, as a click
outside a menu does; so does switching apps. Escape closes it from its
first page, and goes back from Settings. The panel always opens on its
first page with nothing open; `--open` and a clicked notification then set
what's open.

## Settings

Settings is the panel's second page, with three tabs. Changes take effect
at once. Once the engine has stopped for good, after a newer Turnscope
wrote the data ([architecture.md](architecture.md#data)), the switches
for notifications, agents and accounts are off, and so are the panel's
Hide and Show.

| Tab | Holds |
|---|---|
| General | **Menu bar**: Show (All in use, or Most urgent) and Open at login. **Notify me when**: a switch for each kind of alert. **Updates**: the version, Check now and Check automatically |
| Agents | Each installed agent, with a switch that connects or disconnects it, and under its name that it runs another copy of Turnscope or why connecting failed |
| Accounts | Subscriptions, then API keys, each with a switch to show or hide it |

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

An app preference goes in `preferences.rs`; a setting the engine acts on
goes in `alerts::Settings`, and Settings sends it with `engine::ask`.

## Notifications

`notifier.rs` posts every alert the engine raises: the engine has already
dropped the kinds turned off and alerts for hidden accounts. macOS posts
notifications only for an app bundle, so the app run with `cargo run`
posts none.

- **Permission.** It asks when the app starts. If notifications are off
  for Turnscope in System Settings, Settings → General says so and offers
  to open them.
- **Words.** The title is the account, with its label only when another
  account in use has the same title. The subtitle is what happened, and
  the body is when it's back or what to do. Times are clock times, since a
  notification stays on screen.
- **Replacing.** Each is posted under the alert's `replaces`, so a newer
  one for the same limit or account replaces the older: "back" replaces
  "used up". A sign-in notification is removed once the account can be
  read again.
- **Urgency.** Every notification is at the normal level, so none breaks
  through Focus: time-sensitive delivery needs an entitlement the app
  doesn't have.
- **Acknowledging.** The app acknowledges an alert only once macOS accepts
  the notification. One macOS refused comes again when the app next
  starts.
- **Clicking** a notification opens the panel with its account open, once
  the first status has arrived. Just after launch, the panel waits up to 2
  seconds for the item to have a place in the menu bar; with none by then,
  as when the item is hidden, it opens at the screen's top right.

## Updates

`updater.rs` uses Sparkle. It reads the appcast at `SUFeedURL`, the latest
GitHub release's `appcast.xml`, checks each update's signature with
`SUPublicEDKey`, and asks before installing.
`SUVerifyUpdateBeforeExtraction` has it check the disk image's signature
before mounting it. Automatic checks are on by default, and Settings →
General → Check automatically turns them off.

The app has no window to bring forward, so an update a scheduled check
finds would open behind other apps. Instead the footer shows "Update
available" (Sparkle's gentle reminders) until that update is installed,
skipped or put off.

Sparkle is loaded from the bundle when the app starts, not linked, so the
app also runs with `cargo run`. It's loaded only when `SUPublicEDKey` is
set, which only the release build does
([development.md](development.md#release)).

## Opening at login

Settings → General → Open at login registers the app with
`SMAppService.mainApp`. If macOS holds it for approval, the switch stays on
and a row offers to open Login Items in System Settings. If registering
fails, the Mac beeps.

## Logos

`build.rs` builds every SVG in `assets/logos/` into the app
([its README](../crates/turnscope-macos/assets/logos/README.md)). The panel
tints the one-color logos like text, and the menu bar item like its
figures. An account or agent with no logo gets an SF Symbol in the panel
(a key or a terminal) and none in the menu bar.

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
  `crates/turnscope/tests/fixtures/status.json` has it. Its words are as of
  the status's `at`, so it reads the same whenever it's drawn. Changes made
  in it show on screen and go nowhere: nothing is written to the app's
  defaults, the login items or Sparkle's settings, and neither Sparkle nor
  notifications start.
- **`--snapshot`** draws the status in `--fixture`, or else the status the
  engine works out from `--data` and `--home`, reading only: it makes no
  database, raises no alert and catches nothing up. With `--open`, it
  opens a row or the fold first, or draws Settings at the tab named. It
  draws the window background where the panel has glass, the panel at its
  full height, and no sessions under an open account.

The engine runs in the app, so `TURNSCOPE_OFFLINE` and `TURNSCOPE_NOW`
apply to it as to the command line ([cli.md](cli.md#environment)). The app
writes what goes wrong to `app.log` in the data directory, which
`turnscope doctor` shows.
