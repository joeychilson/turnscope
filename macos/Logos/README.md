# Logos

The marks of the agents and subscriptions Turnscope shows. They are trademarks
of their owners, used only to name their products.

- `providers/` holds the logo of every provider models.dev lists, as it
  draws them in one color (`currentColor`), named by the provider's models.dev
  id, from `https://models.dev/logos/<id>.svg`. They stand for agents and
  subscriptions (`anthropic`, `openai`, `opencode`, `opencode-go`, `xai`) and
  for models, by the provider that served them. `scripts/catalog.sh` fetches
  them with the catalog, leaving out any drawn from a picture, which can't be
  drawn in one color, and the placeholder models.dev answers with for a
  provider it has no logo for, and removes those of providers it no longer
  lists; last on 2026-09-25, with the 13 placeholders it then kept removed
  on 2026-09-27.
- `agents/pi.svg` is Pi's logo from https://pi.dev/logo-auto.svg, redrawn in
  one color at three opacities in place of its three colors.

A new agent brings its logo to `agents/` when no provider's stands for it,
under the id `Turnscope/Logos.swift` names it by.
