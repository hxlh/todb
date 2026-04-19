#[derive(Debug, Clone)]
pub struct BuildVersion {
    pub commit_short: String,
    pub build_time: String,
}

pub fn current_build_version() -> BuildVersion {
    BuildVersion {
        commit_short: option_env!("TODB_GIT_COMMIT_SHORT")
            .unwrap_or("unknown")
            .to_string(),
        build_time: option_env!("TODB_BUILD_TIME")
            .unwrap_or("00000000000000")
            .to_string(),
    }
}

pub fn format_version_string(commit_short: &str, build_time: &str) -> String {
    format!("{commit_short}-{build_time}")
}

#[cfg(test)]
mod tests {
    use super::format_version_string;

    #[test]
    fn test_formats_short_sha_and_build_time() {
        let rendered = format_version_string("abc1234", "20260418215711");
        assert_eq!(rendered, "abc1234-20260418215711");
    }

    #[test]
    fn test_keeps_unknown_fallback_when_commit_missing() {
        let rendered = format_version_string("unknown", "00000000000000");
        assert_eq!(rendered, "unknown-00000000000000");
    }
}
