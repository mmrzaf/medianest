# Translations

MediaNest uses gettext. To translate it into a new language:

1. Run `scripts/update-pot.sh` to refresh `medianest.pot`.
2. Create your file: `msginit -i po/medianest.pot -o po/xx.po -l xx`.
3. Translate `po/xx.po` and add `xx` to `po/LINGUAS`.

`scripts/install.sh` compiles every listed language into
`share/locale/xx/LC_MESSAGES/medianest.mo`.
