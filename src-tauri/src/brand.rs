//! Product identity used at runtime. Renaming the app starts here; bundle
//! names and icons live in `tauri.conf.json`.

pub const NAME: &str = "lowyap";
pub const HOMEPAGE: &str = "https://github.com/mohdashraf010897/lowyap";
pub const USER_AGENT: &str = concat!(
    "lowyap/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/mohdashraf010897/lowyap)"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_matches_name_and_homepage() {
        assert!(USER_AGENT.starts_with(&format!("{NAME}/")));
        assert!(USER_AGENT.contains(HOMEPAGE));
        assert!(!USER_AGENT.to_lowercase().contains("handy"));
    }

    #[test]
    fn bundle_config_does_not_use_upstream_updater_or_signing() {
        let conf = include_str!("../tauri.conf.json");
        assert!(
            !conf.contains("cjpais"),
            "tauri.conf.json references upstream"
        );
        assert!(
            !conf.contains("signCommand"),
            "signing is per-build, not default"
        );
        assert!(conf.contains(&format!("{HOMEPAGE}/releases/latest/download/latest.json")));
    }

    #[test]
    fn bundle_name_and_binary_match_brand() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(conf["productName"], NAME);
        assert_eq!(conf["mainBinaryName"], NAME);
        assert!(!conf["identifier"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("handy"));
    }

    #[test]
    fn frontend_brand_mirrors_runtime_brand() {
        let ts = include_str!("../../src/lib/brand.ts");
        assert!(ts.contains(&format!("BRAND_NAME = \"{NAME}\"")));
        assert!(ts.contains(&format!("BRAND_HOMEPAGE = \"{HOMEPAGE}\"")));
    }

    /// Handy is credited by name only in the acknowledgments block.
    #[test]
    fn english_strings_mention_handy_only_as_credit() {
        let mut en: serde_json::Value =
            serde_json::from_str(include_str!("../../src/i18n/locales/en/translation.json"))
                .unwrap();
        let credit = en["settings"]["about"]["acknowledgments"]
            .as_object_mut()
            .unwrap()
            .remove("handy");
        assert!(credit.is_some(), "Handy credit missing");
        assert!(!en.to_string().contains("Handy"));
    }
}
