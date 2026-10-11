#!/bin/sh
# Regenerates po/medianest.pot from the application's source.
# Rust is close enough to C for xgettext's purposes: tr("…"), trf("…", …)
# and trn("…", "…", n) are the translatable calls.
set -eu
cd "$(dirname "$0")/.."
find crates/medianest-app/src -name '*.rs' | sort > po/POTFILES
xgettext --language=Desktop --output=po/medianest.pot \
    data/io.github.mmrzaf.MediaNest.desktop
# Lifetimes such as 'a look like unterminated characters to the C parser;
# those warnings are harmless and filtered out.
xgettext \
    --join-existing \
    --language=C \
    --from-code=UTF-8 \
    --keyword=tr \
    --keyword=trf \
    --keyword=trn:1,2 \
    --add-comments=Translators \
    --package-name=medianest \
    --package-version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)" \
    --msgid-bugs-address=https://github.com/mmrzaf/medianest/issues \
    --files-from=po/POTFILES \
    --output=po/medianest.pot 2>&1 | grep -v 'unterminated character constant' >&2 || true
echo "Wrote po/medianest.pot ($(grep -c '^msgid' po/medianest.pot) messages)"
