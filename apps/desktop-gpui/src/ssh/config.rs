//! SSH host discovery: `discoverSshHosts` in `packages/ssh/src/config.ts`. Reads
//! `~/.ssh/config` (following `Include`) and `~/.ssh/known_hosts` without running ssh.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Where a discovered host came from (`DesktopDiscoveredSshHost.source`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostSource {
    SshConfig,
    KnownHosts,
}

/// `DesktopDiscoveredSshHost`: an alias and the hostname it resolves to, when the config says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredHost {
    pub alias: String,
    pub hostname: String,
    pub source: HostSource,
}

/// The patterns a `Host` line matched, or `None` where the config is uncertain (`Match`, bad quoting).
type Condition = Option<Vec<String>>;

struct HostNameRule {
    conditions: Vec<Condition>,
    hostname: Option<String>,
}

/// `parseSshConfigLine`: OpenSSH's readconf keyword, an optional `=`, then arguments split like
/// `argv_split()`. `None` arguments mean the quoting was invalid.
fn parse_line(line: &str) -> Option<(String, Option<Vec<String>>)> {
    let line = line.trim_start();
    let end = line.find(|c: char| c.is_whitespace() || c == '=').unwrap_or(line.len());
    if end == 0 {
        return None;
    }
    let directive = line[..end].to_lowercase();
    let mut rest = line[end..].trim_start();
    if let Some(after) = rest.strip_prefix('=') {
        rest = after.trim_start();
    }
    let mut args = Vec::new();
    let mut token: Option<String> = None;
    let mut quote: Option<char> = None;
    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        let next = chars.peek().copied();
        if c == '\\' && matches!(next, Some('"' | '\'' | '\\')) || (c == '\\' && quote.is_none() && next == Some(' ')) {
            token.get_or_insert_with(String::new).extend(next);
            chars.next();
        } else if let Some(open) = quote {
            if c == open {
                quote = None;
            } else {
                token.get_or_insert_with(String::new).push(c);
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
            token.get_or_insert_with(String::new);
        } else if c.is_whitespace() {
            args.extend(token.take());
        } else if c == '#' && token.is_none() {
            break;
        } else {
            token.get_or_insert_with(String::new).push(c);
        }
    }
    if quote.is_some() {
        return Some((directive, None));
    }
    args.extend(token);
    Some((directive, Some(args)))
}

fn has_pattern(value: &str) -> bool {
    value.contains('*') || value.contains('?') || value.starts_with('!')
}

/// `*` and `?` globbing over whole strings.
fn glob_matches(pattern: &str, text: &str) -> bool {
    let (pattern, text): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(&c) if c == '?' || c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((star_p, star_t)) => {
                    p = star_p + 1;
                    t = star_t + 1;
                    star = Some((star_p, star_t + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

fn matches_host_patterns(alias: &str, patterns: &[String]) -> bool {
    let mut matched = false;
    for pattern in patterns {
        let (negated, host_pattern) = match pattern.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, pattern.as_str()),
        };
        let matches = host_pattern == "*"
            || if has_pattern(host_pattern) { glob_matches(host_pattern, alias) } else { host_pattern == alias };
        if !matches {
            continue;
        }
        if negated {
            return false;
        }
        matched = true;
    }
    matched
}

/// The first `HostName` that applies to `alias`, when it can be known without running ssh.
fn configured_hostname(alias: &str, rules: &[HostNameRule]) -> Option<String> {
    for rule in rules {
        let skipped = rule
            .conditions
            .iter()
            .any(|condition| condition.as_ref().is_some_and(|c| !matches_host_patterns(alias, c)));
        if skipped {
            continue;
        }
        // SSH takes the first value; an uncertain earlier value blocks later fallbacks.
        let hostname = rule.hostname.as_ref().filter(|_| rule.conditions.iter().all(Option::is_some))?;
        let mut tokens = hostname.split('%').skip(1);
        if tokens.any(|after| !after.starts_with('h')) || hostname.contains('$') {
            return None;
        }
        return Some(hostname.replace("%h", alias));
    }
    None
}

fn expand_home(input: &str, home: &Path) -> PathBuf {
    match input.strip_prefix('~') {
        Some("") => home.to_path_buf(),
        Some(rest) if rest.starts_with(['/', '\\']) => home.join(&rest[1..]),
        _ => PathBuf::from(input),
    }
}

/// The files an `Include` pattern names; only the last segment may hold wildcards.
fn expand_glob(pattern: &Path) -> Vec<PathBuf> {
    let base = pattern.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    if !base.contains(['*', '?']) {
        return if pattern.exists() { vec![pattern.to_path_buf()] } else { Vec::new() };
    }
    let Some(directory) = pattern.parent() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(directory) else { return Vec::new() };
    let mut matched: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| (!name.starts_with('.') || base.starts_with('.')) && glob_matches(&base, name))
        .map(|name| directory.join(name))
        .filter(|path| path.exists())
        .collect();
    matched.sort();
    matched
}

struct Collector<'a> {
    home: &'a Path,
    visited: HashSet<PathBuf>,
    aliases: HashSet<String>,
    rules: Vec<HostNameRule>,
}

impl Collector<'_> {
    fn collect(&mut self, path: &Path, inherited: &[Condition]) {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        if self.visited.contains(&path) {
            // Read each file once, but never reuse its rules under a different Include context.
            self.rules.push(HostNameRule { conditions: inherited.to_vec(), hostname: None });
            return;
        }
        if !path.is_file() {
            return;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else { return };
        self.visited.insert(path);
        let mut condition: Condition = Some(vec!["*".into()]);
        for line in raw.lines() {
            let Some((directive, args)) = parse_line(line) else { continue };
            let mut conditions = inherited.to_vec();
            conditions.push(condition.clone());
            match directive.as_str() {
                "host" => {
                    for alias in args.iter().flatten() {
                        if !alias.is_empty() && !has_pattern(alias) {
                            self.aliases.insert(alias.clone());
                        }
                    }
                    condition = args;
                }
                "match" => condition = None,
                "hostname" => {
                    let hostname = args.filter(|args| args.len() == 1).and_then(|mut args| args.pop());
                    self.rules.push(HostNameRule { conditions, hostname });
                }
                "include" => self.include(args, &conditions),
                _ => {}
            }
        }
    }

    fn include(&mut self, args: Option<Vec<String>>, conditions: &[Condition]) {
        let Some(patterns) = args else {
            self.rules.push(HostNameRule { conditions: conditions.to_vec(), hostname: None });
            return;
        };
        for pattern in patterns {
            let expanded = expand_home(&pattern, self.home);
            let resolved = if expanded.is_absolute() { expanded } else { self.home.join(".ssh").join(expanded) };
            let tilde_user = pattern.starts_with('~') && !(pattern == "~" || pattern[1..].starts_with(['/', '\\']));
            let directory_pattern = resolved.parent().is_some_and(|dir| has_pattern(&dir.to_string_lossy()));
            if pattern.contains('%')
                || pattern.contains("${")
                || pattern.contains('[')
                || tilde_user
                || directory_pattern
            {
                self.rules.push(HostNameRule { conditions: conditions.to_vec(), hostname: None });
                continue;
            }
            for included in expand_glob(&resolved) {
                self.collect(&included, conditions);
            }
        }
    }
}

/// `normalizeKnownHostsHostname`: `[host]:port` and `host:port` to the host.
fn known_hosts_hostname(raw: &str) -> &str {
    if let Some(inner) = raw.strip_prefix('[')
        && let Some((host, port)) = inner.split_once("]:")
        && !port.is_empty()
        && port.bytes().all(|b| b.is_ascii_digit())
    {
        return host;
    }
    match (raw.find(':'), raw.rfind(':')) {
        (Some(first), Some(last)) if first == last => &raw[..last],
        _ => raw,
    }
}

/// `parseKnownHostsHostnames`: plain hostnames, sorted, without hashed entries or patterns.
pub fn parse_known_hosts(raw: &str) -> Vec<String> {
    let mut hostnames = std::collections::BTreeSet::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut fields = trimmed.split_whitespace();
        if trimmed.starts_with('@') {
            fields.next();
        }
        let Some(host_field) = fields.next().filter(|field| !field.starts_with('|')) else { continue };
        for raw_host in host_field.split(',') {
            let host = known_hosts_hostname(raw_host).trim();
            if !host.is_empty() && !has_pattern(host) {
                hostnames.insert(host.to_owned());
            }
        }
    }
    sorted(hostnames.into_iter().collect())
}

/// `localeCompare` order, near enough for host names: case-insensitive first.
fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b)));
    values
}

/// `discoverSshHosts` for the home directory `home`. Blocks on the files.
pub fn discover(home: &Path) -> Vec<DiscoveredHost> {
    let ssh = home.join(".ssh");
    let mut collector = Collector { home, visited: HashSet::new(), aliases: HashSet::new(), rules: Vec::new() };
    collector.collect(&ssh.join("config"), &[]);
    let known_hosts = std::fs::read_to_string(ssh.join("known_hosts")).map(|raw| parse_known_hosts(&raw));
    let mut discovered = BTreeMap::new();
    let mut configured = HashSet::new();
    for alias in &collector.aliases {
        let hostname = configured_hostname(alias, &collector.rules);
        if let Some(hostname) = &hostname {
            configured.insert(hostname.to_lowercase());
        }
        let hostname = hostname.unwrap_or_else(|| alias.clone());
        discovered
            .insert(alias.clone(), DiscoveredHost { alias: alias.clone(), hostname, source: HostSource::SshConfig });
    }
    for hostname in known_hosts.unwrap_or_default() {
        if discovered.contains_key(&hostname) || configured.contains(&hostname.to_lowercase()) {
            continue;
        }
        let host =
            DiscoveredHost { alias: hostname.clone(), hostname: hostname.clone(), source: HostSource::KnownHosts };
        discovered.insert(hostname, host);
    }
    let order = sorted(discovered.keys().cloned().collect());
    order.into_iter().filter_map(|alias| discovered.remove(&alias)).collect()
}

/// `filterDiscoveredSshHosts`: alias prefix matches first, then alias or hostname substrings.
pub fn filter_hosts<'a>(hosts: &'a [DiscoveredHost], query: &str) -> Vec<&'a DiscoveredHost> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return hosts.iter().collect();
    }
    let (mut prefix, mut substring) = (Vec::new(), Vec::new());
    for host in hosts {
        let alias = host.alias.to_lowercase();
        if alias.starts_with(&query) {
            prefix.push(host);
        } else if alias.contains(&query) || host.hostname.to_lowercase().contains(&query) {
            substring.push(host);
        }
    }
    prefix.extend(substring);
    prefix
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch home in the temp directory, removed on drop.
    struct Home(PathBuf);

    impl Home {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("t3-ssh-config-{name}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(dir.join(".ssh")).expect("scratch home");
            Self(dir)
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("scratch dir");
            std::fs::write(path, contents).expect("scratch file");
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            drop(std::fs::remove_dir_all(&self.0));
        }
    }

    fn names(hosts: &[DiscoveredHost]) -> Vec<(&str, &str, HostSource)> {
        hosts.iter().map(|h| (h.alias.as_str(), h.hostname.as_str(), h.source)).collect()
    }

    #[test]
    fn lines_split_like_openssh() {
        assert_eq!(parse_line("  Host = a b"), Some(("host".into(), Some(vec!["a".into(), "b".into()]))));
        assert_eq!(parse_line("HostName \"my host\" # note"), Some(("hostname".into(), Some(vec!["my host".into()]))));
        assert_eq!(parse_line(r"IdentityFile a\ b"), Some(("identityfile".into(), Some(vec!["a b".into()]))));
        assert_eq!(parse_line("Host 'open"), Some(("host".into(), None)));
        assert_eq!(parse_line("   "), None);
    }

    #[test]
    fn globs_match_whole_strings() {
        assert!(glob_matches("*.example.com", "box.example.com"));
        assert!(glob_matches("dev?", "dev1"));
        assert!(!glob_matches("dev?", "dev12"));
        assert!(glob_matches("*", ""));
    }

    #[test]
    fn config_aliases_resolve_hostnames_and_skip_known_duplicates() {
        let home = Home::new("discover");
        home.write(
            ".ssh/config",
            "Host devbox\n  HostName 10.0.0.5\nHost *.lab !skip\n  User me\nHost tpl\n  HostName %h.internal\n\
             Host weird\n  HostName %r.example\nInclude conf.d/*.conf\nMatch host x\n  HostName nope\n",
        );
        home.write(".ssh/conf.d/a.conf", "Host included\n");
        home.write(".ssh/conf.d/.hidden.conf", "Host hidden\n");
        home.write(
            ".ssh/known_hosts",
            "10.0.0.5 ssh-ed25519 AAA\n[box.lan]:2222,1.2.3.4 ssh-rsa B\n|1|hash= ssh-rsa C\n",
        );
        let hosts = discover(&home.0);
        assert_eq!(
            names(&hosts),
            [
                ("1.2.3.4", "1.2.3.4", HostSource::KnownHosts),
                ("box.lan", "box.lan", HostSource::KnownHosts),
                ("devbox", "10.0.0.5", HostSource::SshConfig),
                ("included", "included", HostSource::SshConfig),
                ("tpl", "tpl.internal", HostSource::SshConfig),
                ("weird", "weird", HostSource::SshConfig),
            ]
        );
    }

    #[test]
    fn a_match_block_hides_later_hostnames() {
        let home = Home::new("match");
        home.write(".ssh/config", "Match all\n  HostName a\nHost late\n  HostName b\n");
        assert_eq!(names(&discover(&home.0)), [("late", "late", HostSource::SshConfig)]);
    }

    #[test]
    fn a_missing_home_discovers_nothing() {
        assert!(discover(Path::new("/definitely/not/a/home")).is_empty());
    }

    #[test]
    fn known_hosts_drop_ports_hashes_and_patterns() {
        assert_eq!(
            parse_known_hosts("@cert-authority *.x ssh-rsa A\nb.example:22 k\n::1 k\n# c\n Zed,alpha k\n"),
            ["::1", "alpha", "b.example", "Zed"]
        );
    }

    #[test]
    fn filtering_puts_alias_prefixes_first() {
        let host = |alias: &str, hostname: &str| DiscoveredHost {
            alias: alias.into(),
            hostname: hostname.into(),
            source: HostSource::SshConfig,
        };
        let hosts = [host("lab-dev", "1.1.1.1"), host("dev", "2.2.2.2"), host("prod", "dev.example")];
        let aliases = |query| filter_hosts(&hosts, query).iter().map(|h| h.alias.as_str()).collect::<Vec<_>>();
        assert_eq!(aliases("DEV"), ["dev", "lab-dev", "prod"]);
        assert_eq!(aliases(" "), ["lab-dev", "dev", "prod"]);
        assert!(aliases("zzz").is_empty());
    }
}
