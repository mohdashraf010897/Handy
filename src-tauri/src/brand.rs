//! Product identity used at runtime. Canonical source lives in
//! `design/brand/brand.json`; unit tests below enforce parity across Rust,
//! TypeScript, `tauri.conf.json`, `index.html`, and `site/index.html`.

pub const NAME: &str = "lowyap";
pub const TAGLINE: &str = "lowkey yap. it types.";
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
    fn brand_json_stays_in_sync_across_stack() {
        let brand: serde_json::Value =
            serde_json::from_str(include_str!("../../design/brand/brand.json")).unwrap();
        assert_eq!(brand["name"], NAME);
        assert_eq!(brand["tagline"], TAGLINE);
        assert_eq!(brand["homepage"], HOMEPAGE);

        let html = include_str!("../../index.html");
        assert!(
            html.contains(&format!("<title>{NAME}</title>")),
            "index.html title does not match brand name"
        );

        let site = include_str!("../../site/index.html");
        assert!(site.contains(TAGLINE), "site/index.html missing tagline");
        let vermilion_ui = brand["colors"]["vermilionUi"]
            .as_str()
            .unwrap()
            .to_ascii_lowercase();
        assert!(
            site.contains(&vermilion_ui),
            "site/index.html primary palette drifted from brand.json"
        );
    }

    #[test]
    fn bundle_config_does_not_use_upstream_updater_or_signing() {
        let conf_raw = include_str!("../tauri.conf.json");
        assert!(
            !conf_raw.contains("cjpais"),
            "tauri.conf.json references upstream"
        );
        assert!(
            !conf_raw.contains("signCommand"),
            "signing is per-build, not default"
        );
        assert!(conf_raw.contains(&format!("{HOMEPAGE}/releases/latest/download/latest.json")));

        let conf: serde_json::Value = serde_json::from_str(conf_raw).unwrap();
        let csp = conf["app"]["security"]["csp"].as_str().unwrap_or_default();
        assert!(
            csp.contains("default-src 'self'") && csp.contains("object-src 'none'"),
            "tauri.conf.json must enforce a non-null CSP"
        );
        let allow = conf["app"]["security"]["assetProtocol"]["scope"]["allow"]
            .as_array()
            .expect("assetProtocol.scope.allow must be an array");
        assert!(
            !allow.iter().any(|entry| entry.as_str() == Some("**")),
            "assetProtocol.scope.allow must not expose wildcard '**'"
        );
        assert_eq!(
            conf["app"]["security"]["assetProtocol"]["scope"]["requireLiteralLeadingDot"], true,
            "assetProtocol must block hidden dotfile traversal"
        );
    }

    #[test]
    fn bundle_name_and_binary_match_brand() {
        let brand: serde_json::Value =
            serde_json::from_str(include_str!("../../design/brand/brand.json")).unwrap();
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(conf["productName"], NAME);
        assert_eq!(conf["mainBinaryName"], NAME);
        assert_eq!(conf["identifier"], brand["identifier"]);
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
        assert!(ts.contains(&format!("BRAND_TAGLINE = \"{TAGLINE}\"")));
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
