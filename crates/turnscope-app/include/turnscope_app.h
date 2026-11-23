#ifndef TURNSCOPE_APP_H
#define TURNSCOPE_APP_H

#include <stdint.h>

typedef struct TurnscopeApp TurnscopeApp;
typedef void (*TurnscopeWakeFn)(void *ctx);

/* Build a one-shot panel snapshot JSON for `--snapshot` (fixture or DB).
 * Returns JSON `{ "ok": PanelModel }` or `{ "error": "...", "code": 1|2 }`. */
char *turnscope_app_snapshot(const char *config_json);

/* Start a live or fixture app core. `wake` may be invoked from any thread.
 * On error (e.g. unreadable fixture), writes error text to `error_out` (if non-null) and returns NULL. */
TurnscopeApp *turnscope_app_start(
    const char *config_json,
    void *ctx,
    TurnscopeWakeFn wake,
    char **error_out
);

/* Drain pending side effects and return the current `AppSync` JSON. */
char *turnscope_app_sync(TurnscopeApp *app);

/* Apply a JSON `Action` and return the updated `AppSync` JSON. */
char *turnscope_app_action(TurnscopeApp *app, const char *action_json);

/* Append a line to `app.log` in the active data directory (no-op in fixture mode). */
void turnscope_app_log(TurnscopeApp *app, const char *line);

/* Build a plain-text diagnostics report (version, doctor output, recent app.log)
 * from a JSON SnapshotConfig (`fixture`/`data`/`home`). Safe to call off the main thread. */
char *turnscope_app_diagnostics(const char *config_json);

/* Return the path to `app.log` (or the data directory if none exists yet), or NULL in fixture mode. */
char *turnscope_app_log_path(TurnscopeApp *app);

/* Stop the background engine and free the app core. */
void turnscope_app_stop(TurnscopeApp *app);

/* Free any NUL-terminated string returned by `turnscope_app_*`. */
void turnscope_app_free_string(char *text);

#endif
