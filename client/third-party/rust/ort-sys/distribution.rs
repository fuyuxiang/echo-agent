pub fn find_dist<'a>(table: &'a str, target: &str, feature_set: &str) -> Option<(&'a str, &'a str)> {
    table
        // Git's Windows checkout can use CRLF. A newline split retains the
        // carriage return in the SHA256 field and breaks hexadecimal parsing.
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.split('\t').collect::<Vec<_>>())
        .find(|columns| columns[0] == feature_set && columns[1] == target)
        .map(|columns| (columns[2], columns[3]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_distribution_urls_and_digests_with_lf_and_crlf() {
        let table = include_str!("dist.txt");
        let windows = table.replace("\r\n", "\n").replace('\n', "\r\n");
        for line in table.lines().filter(|line| !line.is_empty() && !line.starts_with('#')) {
            let columns: Vec<_> = line.split('\t').collect();
            let expected = Some((columns[2], columns[3]));
            assert_eq!(find_dist(table, columns[1], columns[0]), expected);
            assert_eq!(find_dist(&windows, columns[1], columns[0]), expected);
            let hash = find_dist(&windows, columns[1], columns[0]).unwrap().1;
            assert_eq!(hash.len(), 64);
            assert!(hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn does_not_substitute_another_target_or_provider() {
        let table = include_str!("dist.txt");
        assert!(find_dist(table, "unknown-target", "none").is_none());
        assert!(find_dist(table, "x86_64-pc-windows-msvc", "unknown-provider").is_none());
    }
}
