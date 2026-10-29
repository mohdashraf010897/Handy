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
}
