#!/usr/bin/env bash
# Fails when UI code reaches for literals the theme owns. Use Palette fields and theme::tokens instead.
# Allowed in src/theme.rs (defines them), src/schemes.rs (raw colour data), src/ui/ (the primitives)
# and src/nsh/ (NSH screens; checked separately below against theme::tokens and the helpers in src/nsh/mod.rs).
set -u
cd "$(dirname "$0")/.."
pattern='\.rounded(_[a-z]{1,2})?_(sm|md|lg|xl|2xl|full)\(\)|\.opacity\(0?\.[0-9]|\.rounded\((gpui::)?px\(|hsla\(|\.text_size\(rems\(|\.h\(rems\(|\.h\($'
bad=$(grep -rnE "$pattern" src --include=*.rs | grep -vE '^src/(theme\.rs|schemes\.rs|ui/|nsh/)' || true)
if [ -n "$bad" ]; then
    echo "Design literals outside src/theme.rs and src/ui/ (use Palette radii/colors or theme::tokens):"
    echo "$bad"
    exit 1
fi

# NSH screens take their literals from src/nsh/mod.rs and theme::tokens too.
nsh_pattern='\.rounded_full\(\)|Points\((999|2)\.\)|\.opacity\(0?\.[2-9]|text_size\((super::)?text_(xs|sm)\(\)\)\.text_color\((super::)?Colors::(muted|foreground)\(\)\)'
nsh_bad=$(grep -rnE "$nsh_pattern" src/nsh --include=*.rs | grep -v '^src/nsh/mod.rs' || true)
if [ -n "$nsh_bad" ]; then
    echo "Design literals in src/nsh/ (use caption/note/body/small, pill(), swatch_radius() or theme::tokens):"
    echo "$nsh_bad"
    exit 1
fi
