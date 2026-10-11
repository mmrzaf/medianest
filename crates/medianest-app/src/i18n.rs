//! Translations through gettext.

use gettextrs::{LocaleCategory, bind_textdomain_codeset, bindtextdomain, setlocale, textdomain};

const DOMAIN: &str = "medianest";

/// Where compiled translations are installed, relative to the executable's
/// prefix: `<prefix>/bin/medianest` reads `<prefix>/share/locale`.
fn locale_dir() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("MEDIANEST_LOCALEDIR") {
        return dir.into();
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.parent()?
                .parent()
                .map(|prefix| prefix.join("share/locale"))
        })
        .unwrap_or_else(|| "/usr/share/locale".into())
}

pub fn init() {
    setlocale(LocaleCategory::LcAll, "");
    let _ = bindtextdomain(DOMAIN, locale_dir());
    let _ = bind_textdomain_codeset(DOMAIN, "UTF-8");
    let _ = textdomain(DOMAIN);
}

/// Translates a message.
pub fn tr(message: &str) -> String {
    gettextrs::gettext(message)
}

/// Translates a message with a count, picking the plural form, and
/// substitutes `{}` with the count.
pub fn trn(singular: &str, plural: &str, count: u64) -> String {
    let n = u32::try_from(count).unwrap_or(u32::MAX);
    gettextrs::ngettext(singular, plural, n).replacen("{}", &crate::util::number(count), 1)
}

/// Translates a message and fills `{}` placeholders in order.
pub fn trf(message: &str, args: &[&str]) -> String {
    let mut text = gettextrs::gettext(message);
    for arg in args {
        text = text.replacen("{}", arg, 1);
    }
    text
}
