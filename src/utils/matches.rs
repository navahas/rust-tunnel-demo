use wildmatch::WildMatch;

pub fn matches(pattern: &str, domain: &str) -> bool {
    WildMatch::new(pattern).matches(domain)
}
