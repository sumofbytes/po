#!/usr/bin/env bash
# Fails when UI code reaches for literals the theme owns. Use Palette fields and theme::tokens instead.
# Allowed in src/theme.rs (defines them) and src/ui/ (the primitives).
set -u
cd "$(dirname "$0")/.."
pattern='\.rounded_(sm|md|lg|xl|2xl|full)\(\)|\.rounded\((gpui::)?px\(|hsla\(|\.text_size\(rems\(|\.h\(rems\('
bad=$(grep -rnE "$pattern" src --include=*.rs | grep -vE '^src/(theme\.rs|schemes\.rs|ui/)' || true)
if [ -n "$bad" ]; then
    echo "Design literals outside src/theme.rs and src/ui/ (use Palette radii/colors or theme::tokens):"
    echo "$bad"
    exit 1
fi
