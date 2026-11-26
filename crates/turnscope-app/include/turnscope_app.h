#ifndef TURNSCOPE_APP_H
#define TURNSCOPE_APP_H

#ifdef __cplusplus
extern "C" {
#endif

typedef struct TurnscopeApp TurnscopeApp;
typedef void (*TurnscopeWakeFn)(void *ctx);

/* Build a one-shot panel snapshot JSON for `--snapshot` (fixture or DB).
 * Returns JSON `{ "ok": { "panel": PanelModel } }` or `{ "err": { "error": "...", "code": 1|2 } }`,
 * or NULL on panic. */
char *turnscope_app_snapshot(const char *config_json);

/* Start a live or fixture app core. Pair with `turnscope_app_stop` before releasing `ctx`.
 * `wake` may be invoked from any engine thread while an internal engine lock is held; it must
 * dispatch asynchronously to the host's event loop without blocking or calling back into
 * `turnscope_app_*` synchronously.
 * When `error_out` is non-null, `*error_out` is set to NULL on success; on error it receives a
 * NUL-terminated error string (free with `turnscope_app_free_string`) and NULL is returned. */
TurnscopeApp *turnscope_app_start(
    const char *config_json,
    void *ctx,
    TurnscopeWakeFn wake,
    char **error_out
);

/* Clear `wake` so it is never called again, stop the engine, and free `app`. No-op if NULL. */
void turnscope_app_stop(TurnscopeApp *app);

/* Drain pending side effects and return the current `AppSync` JSON, or NULL on error/panic. */
char *turnscope_app_sync(TurnscopeApp *app);

/* Apply a JSON `Action` and return the updated `AppSync` JSON, or NULL on error/panic. */
char *turnscope_app_action(TurnscopeApp *app, const char *action_json);

/* Append a line to `app.log` in the active data directory (no-op in fixture mode). */
void turnscope_app_log(TurnscopeApp *app, const char *line);

/* Build a plain-text diagnostics report (version, doctor's notes and the end of app.log)
 * from a JSON SnapshotConfig (`fixture`/`data`/`home`). Safe to call off the main thread. */
char *turnscope_app_diagnostics(const char *config_json);

/* Return the path to `app.log` (or the data directory if none exists yet), or NULL in fixture mode. */
char *turnscope_app_log_path(TurnscopeApp *app);

/* Free any NUL-terminated string returned by `turnscope_app_*`. */
void turnscope_app_free_string(char *text);

#ifdef __cplusplus
}
#endif

#endif
