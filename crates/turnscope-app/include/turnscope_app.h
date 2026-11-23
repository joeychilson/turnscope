#ifndef TURNSCOPE_APP_H
#define TURNSCOPE_APP_H

#include <stdint.h>

typedef struct TurnscopeApp TurnscopeApp;
typedef void (*TurnscopeWakeFn)(void *ctx);

/* Parse process arguments into a JSON `Launch` object. Free with `turnscope_app_free_string`. */
char *turnscope_app_parse_launch(int32_t argc, const char *const *argv);

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

/* Stop the background engine and free the app core. */
void turnscope_app_stop(TurnscopeApp *app);

/* Free any NUL-terminated string returned by `turnscope_app_*`. */
void turnscope_app_free_string(char *text);

#endif
