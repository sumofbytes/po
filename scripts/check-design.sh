#!/usr/bin/env bash
# Fails when UI code reaches for literals the theme owns. Use Palette fields and theme::tokens instead.
# Allowed in src/theme.rs (defines them), src/schemes.rs (raw colour data), src/ui/ (the primitives)
# and src/nsh/ (NSH screens, styled with NSH's own tokens).
set -u
cd "$(dirname "$0")/.."
pattern='\.rounded(_[a-z]{1,2})?_(sm|md|lg|xl|2xl|full)\(\)|\.opacity\(0?\.[0-9]|\.rounded\((gpui::)?px\(|hsla\(|\.text_size\(rems\(|\.h\(rems\(|\.h\($'
bad=$(grep -rnE "$pattern" src --include=*.rs | grep -vE '^src/(theme\.rs|schemes\.rs|ui/|nsh/)' || true)
if [ -n "$bad" ]; then
    echo "Design literals outside src/theme.rs and src/ui/ (use Palette radii/colors or theme::tokens):"
    echo "$bad"
    exit 1
fi
