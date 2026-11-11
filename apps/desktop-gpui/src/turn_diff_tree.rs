//! A turn's changed files as a folder tree (`lib/turnDiffTree.ts`) and the rows it shows
//! (`collectExpandedRows` in `ChangedFilesTree.tsx`).

use std::cmp::Ordering;
use std::collections::HashMap;

use gpui_kit::SharedString;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stat {
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Directory { name: SharedString, path: SharedString, stat: Stat, children: Vec<Self> },
    File { name: SharedString, path: SharedString, stat: Stat },
}

#[cfg(test)]
impl Node {
    pub fn path(&self) -> &SharedString {
        match self {
            Self::Directory { path, .. } | Self::File { path, .. } => path,
        }
    }
}

#[derive(Default)]
struct Folder {
    stat: Stat,
    /// Keyed by name, in first-seen order: sorting happens once the tree is built.
    folders: Vec<(String, Self)>,
    files: Vec<(String, Stat)>,
}

impl Folder {
    fn child(&mut self, name: &str) -> &mut Self {
        let index = match self.folders.iter().position(|(n, _)| n == name) {
            Some(index) => index,
            None => {
                self.folders.push((name.to_owned(), Self::default()));
                self.folders.len() - 1
            }
        };
        &mut self.folders[index].1
    }
}

/// `buildTurnDiffTree`: folders first, then files, each in natural name order; a folder holding
/// only one folder merges into it (`src/app`).
pub fn build(files: &[FileChange]) -> Vec<Node> {
    let mut root = Folder::default();
    for file in files {
        let segments: Vec<&str> = file.path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
        let Some((name, folders)) = segments.split_last() else { continue };
        let stat = Stat { additions: file.additions, deletions: file.deletions };
        let mut folder = &mut root;
        add(&mut folder.stat, stat);
        for segment in folders {
            folder = folder.child(segment);
            add(&mut folder.stat, stat);
        }
        folder.files.push(((*name).to_owned(), stat));
    }
    nodes(&root, "")
}

fn add(total: &mut Stat, stat: Stat) {
    total.additions += stat.additions;
    total.deletions += stat.deletions;
}

fn nodes(folder: &Folder, prefix: &str) -> Vec<Node> {
    let join = |name: &str| if prefix.is_empty() { name.to_owned() } else { format!("{prefix}/{name}") };
    let mut folders: Vec<&(String, Folder)> = folder.folders.iter().collect();
    folders.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    let mut files: Vec<&(String, Stat)> = folder.files.iter().collect();
    files.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    let mut out: Vec<Node> = folders
        .into_iter()
        .map(|(name, child)| {
            let path = join(name);
            let (mut name, mut path, mut child) = (name.clone(), path, child);
            // `compactDirectoryNode`.
            while child.files.is_empty() && child.folders.len() == 1 {
                let (only_name, only) = &child.folders[0];
                name = format!("{name}/{only_name}");
                path = format!("{path}/{only_name}");
                child = only;
            }
            Node::Directory { name: name.into(), children: nodes(child, &path), path: path.into(), stat: child.stat }
        })
        .collect();
    out.extend(files.into_iter().map(|(name, stat)| Node::File {
        name: name.clone().into(),
        path: join(name).into(),
        stat: *stat,
    }));
    out
}

/// `collectExpandedRows`: every node with its depth, skipping the contents of folded folders.
/// `overrides` holds the folders toggled away from `all_expanded`.
pub fn rows<'a>(
    nodes: &'a [Node],
    overrides: &HashMap<SharedString, bool>,
    all_expanded: bool,
) -> Vec<(&'a Node, usize)> {
    fn walk<'a>(
        nodes: &'a [Node],
        overrides: &HashMap<SharedString, bool>,
        all_expanded: bool,
        depth: usize,
        out: &mut Vec<(&'a Node, usize)>,
    ) {
        for node in nodes {
            out.push((node, depth));
            if let Node::Directory { path, children, .. } = node
                && overrides.get(path).copied().unwrap_or(all_expanded)
            {
                walk(children, overrides, all_expanded, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(nodes, overrides, all_expanded, 0, &mut out);
    out
}

/// `Intl.Collator(undefined, {numeric: true, sensitivity: "base"})`, for paths: case-insensitive,
/// with digit runs compared by value.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn digits(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
        let mut run = String::new();
        while let Some(c) = chars.next_if(char::is_ascii_digit) {
            run.push(c);
        }
        run.trim_start_matches('0').to_owned()
    }
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let (x, y) = (digits(&mut a), digits(&mut b));
                let order = x.len().cmp(&y.len()).then_with(|| x.cmp(&y));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                a.next();
                b.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(path: &str, additions: u64, deletions: u64) -> FileChange {
        FileChange { path: path.into(), additions, deletions }
    }

    fn shape(nodes: &[Node]) -> Vec<String> {
        rows(nodes, &HashMap::new(), true)
            .into_iter()
            .map(|(node, depth)| match node {
                Node::Directory { name, stat, .. } => {
                    format!("{}{name}/ +{} -{}", "  ".repeat(depth), stat.additions, stat.deletions)
                }
                Node::File { name, path, .. } => format!("{}{name} ({path})", "  ".repeat(depth)),
            })
            .collect()
    }

    #[test]
    fn folders_lead_sort_naturally_and_merge_single_child_chains() {
        let tree = build(&[
            change("README.md", 1, 0),
            change("src\\app\\b10.rs", 2, 1),
            change("src/app/b2.rs", 3, 0),
            change("docs/guide.md", 0, 4),
            change("a.txt", 1, 1),
        ]);
        assert_eq!(
            shape(&tree),
            [
                "docs/ +0 -4",
                "  guide.md (docs/guide.md)",
                "src/app/ +5 -1",
                "  b2.rs (src/app/b2.rs)",
                "  b10.rs (src/app/b10.rs)",
                "a.txt (a.txt)",
                "README.md (README.md)",
            ]
        );
    }

    #[test]
    fn folded_folders_hide_their_contents_and_overrides_win() {
        let tree = build(&[change("src/a.rs", 1, 0), change("lib/b.rs", 1, 0)]);
        assert_eq!(rows(&tree, &HashMap::new(), false).len(), 2);
        let overrides = HashMap::from([(SharedString::from("src"), true)]);
        let shown: Vec<&str> = rows(&tree, &overrides, false).iter().map(|(node, _)| node.path().as_ref()).collect();
        assert_eq!(shown, ["lib", "src", "src/a.rs"]);
    }

    #[test]
    fn natural_order_ignores_case_and_reads_numbers() {
        let mut names = vec!["b10", "B2", "a", "b1"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["a", "b1", "B2", "b10"]);
    }
}
