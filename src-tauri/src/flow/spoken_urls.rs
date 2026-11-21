//! Spoken URL formatting for dictation.

pub use lowyap_flow::spoken_urls::format_spoken_urls;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_spoken_paths_on_literal_urls() {
        assert_eq!(
            format_spoken_urls("Open https://github.com slash lowyap and star it."),
            "Open https://github.com/lowyap and star it."
        );
    }

    #[test]
    fn leaves_prose_without_a_recognized_domain_unchanged() {
        let prose = "Put a dot on the i.";
        assert_eq!(format_spoken_urls(prose), prose);
    }
}
