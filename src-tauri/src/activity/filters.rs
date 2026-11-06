//! Privacy filters (SPEC §2.4, §5.2): what activity tracking must never record.

/// Apps that are never recorded, matched case-insensitively against the executable name.
pub fn default_excluded_apps() -> Vec<String> {
    [
        "keepass.exe",
        "keepassxc.exe",
        "1password.exe",
        "bitwarden.exe",
        "lastpass.exe",
        "dashlane.exe",
        "enpass.exe",
        "credentialuibroker.exe",
        "consent.exe",
        "logonui.exe",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// URL fragments that are never stored: banking, payments and credential pages.
pub fn default_excluded_url_parts() -> Vec<String> {
    [
        "bank",
        "paypal.",
        "1password.",
        "bitwarden.",
        "lastpass.",
        "/login",
        "/signin",
        "/oauth",
        "accounts.google.",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

pub fn is_app_excluded(app: &str, excluded: &[String]) -> bool {
    let app = app.to_lowercase();
    excluded.iter().any(|e| e.to_lowercase() == app)
}

pub fn is_url_excluded(url: &str, parts: &[String]) -> bool {
    let url = url.to_lowercase();
    parts.iter().any(|p| url.contains(&p.to_lowercase()))
}

/// Browser private-browsing windows announce themselves in the title; their titles (which
/// would reveal what is being browsed) are replaced by a placeholder.
pub fn is_private_browsing_title(title: &str) -> bool {
    let t = title.to_lowercase();
    [
        "inprivate",
        "incognito",
        "private browsing",
        "приватн",
        "инкогнито",
    ]
    .iter()
    .any(|m| t.contains(m))
}

/// The title that may be stored for a window.
pub fn sanitize_title(title: &str) -> String {
    if is_private_browsing_title(title) {
        "(private window)".to_owned()
    } else {
        title.chars().take(300).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apps_and_urls() {
        let apps = default_excluded_apps();
        assert!(is_app_excluded("KeePassXC.exe", &apps));
        assert!(!is_app_excluded("code.exe", &apps));
        let parts = default_excluded_url_parts();
        assert!(is_url_excluded("https://online.mybank.com/home", &parts));
        assert!(is_url_excluded("https://example.com/login?next=/", &parts));
        assert!(!is_url_excluded("https://doc.rust-lang.org/std", &parts));
    }

    #[test]
    fn private_windows_are_masked() {
        assert_eq!(
            sanitize_title("Secret page - Google Chrome (Incognito)"),
            "(private window)"
        );
        assert_eq!(
            sanitize_title("Новая вкладка — приватное окно"),
            "(private window)"
        );
        assert_eq!(
            sanitize_title("main.rs - Visual Studio Code"),
            "main.rs - Visual Studio Code"
        );
        assert_eq!(sanitize_title(&"x".repeat(1000)).len(), 300);
    }
}
