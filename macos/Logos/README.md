# Logos

The marks of the providers and agents Turnscope shows, drawn in one color
(`currentColor`) so the app can tint them like icons. They are trademarks of
their owners, used only to name their products.

- `providers/<id>.svg`: one per provider Turnscope reads, named by its id,
  from `https://models.dev/logos/<id>.svg`. It must be an SVG of shapes, not
  an embedded picture, and not models.dev's placeholder for a provider it
  has no logo for.
- `agents/<id>.svg`: one per agent, named by its id. Claude Code, Codex,
  OpenCode and Grok Build use their makers' marks; Pi's is redrawn in one
  color from https://pi.dev/logo-auto.svg.

A mark is drawn at the size of the frame its `viewBox` sets, so a solid
mark gets a wider `viewBox` to look as large as the others. OpenCode's two
are padded until their ink covers as much of the frame as Anthropic's, 21%,
measured on a render.

An agent or provider without a logo is drawn with a generic mark.
