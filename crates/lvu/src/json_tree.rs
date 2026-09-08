//! A tree over the byte ranges `json_spans::classify` found in a record's
//! text (docs/dialog-system.md §8.11).
//!
//! Nothing here re-serialises, reorders or decodes a value: every scalar the
//! tree hands out is a slice of the original text, keys keep document order,
//! and the only decoding is the one `classify` already does for key
//! identity. The text reaching this module is the display text, which the
//! provider produced with `String::from_utf8_lossy`, so invalid UTF-8 is
//! already the replacement character and stays that way on screen.

use std::ops::Range;

use crate::json_spans::{JsonKind, JsonSpan, classify};

/// Index into `JsonTree::nodes`.
pub type NodeId = usize;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NodeKind {
    /// Entries in document order: decoded key, the key's own span, the value.
    Object(Vec<(String, Range<usize>, NodeId)>),
    Array(Vec<NodeId>),
    Scalar(JsonKind),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Node {
    /// The value's bytes in the original text, container brackets included.
    pub span: Range<usize>,
    pub kind: NodeKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonTree {
    nodes: Vec<Node>,
}

/// One row of a flattened tree, as Details and Fields draw it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeRow {
    /// `a.b[2].c` — the display path, and the key of the expansion memory.
    pub path: String,
    pub depth: usize,
    /// The key, or `[i]` inside an array.
    pub label: String,
    pub node: NodeId,
    pub shape: RowShape,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RowShape {
    Container {
        expanded: bool,
        count: usize,
        array: bool,
    },
    Scalar(JsonKind),
}

impl JsonTree {
    /// `None` unless the whole text is one valid JSON value inside
    /// `classify`'s bounds — the same rule the log line highlights by.
    pub fn parse(text: &str) -> Option<Self> {
        let spans = classify(text)?;
        let mut builder = Builder {
            text,
            spans: &spans,
            at: 0,
            nodes: Vec::new(),
        };
        let root = builder.value()?;
        (root == 0 && builder.at == spans.len()).then_some(JsonTree {
            nodes: builder.nodes,
        })
    }

    pub fn root(&self) -> NodeId {
        0
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    /// Whether the root is an object, i.e. the record has named fields.
    pub fn is_object(&self) -> bool {
        matches!(
            self.nodes.first().map(|node| &node.kind),
            Some(NodeKind::Object(_))
        )
    }

    /// `{3 keys}` / `[12]` — what a collapsed container shows in place of
    /// its bytes. Scalars have no summary: they show their bytes.
    pub fn summary(&self, id: NodeId) -> Option<String> {
        match &self.node(id).kind {
            NodeKind::Object(entries) => Some(match entries.len() {
                0 => "{}".to_owned(),
                1 => "{1 key}".to_owned(),
                n => format!("{{{n} keys}}"),
            }),
            NodeKind::Array(items) => Some(format!("[{}]", items.len())),
            NodeKind::Scalar(_) => None,
        }
    }

    /// The scalar's bytes, verbatim.
    pub fn scalar_text<'a>(&self, text: &'a str, id: NodeId) -> Option<&'a str> {
        match &self.node(id).kind {
            NodeKind::Scalar(_) => text.get(self.node(id).span.clone()),
            _ => None,
        }
    }

    /// A string scalar without its quotes and with JSON escapes decoded, for
    /// comparing and counting values. Display never uses this.
    pub fn scalar_value(&self, text: &str, id: NodeId) -> Option<String> {
        let raw = self.scalar_text(text, id)?;
        match &self.node(id).kind {
            NodeKind::Scalar(JsonKind::String) => decode_string(raw),
            NodeKind::Scalar(_) => Some(raw.to_owned()),
            _ => None,
        }
    }

    /// The node at a display path, walking keys and `[i]` indexes.
    pub fn find(&self, path: &str) -> Option<NodeId> {
        let mut id = self.root();
        for segment in split_path(path) {
            id = match (&self.node(id).kind, segment) {
                (NodeKind::Object(entries), Segment::Key(key)) => {
                    entries.iter().find(|(k, _, _)| *k == key)?.2
                }
                (NodeKind::Array(items), Segment::Index(index)) => *items.get(index)?,
                _ => return None,
            };
        }
        Some(id)
    }

    /// The rows a viewer draws: the root's entries always, and a container's
    /// children only while `expanded(path)` says so. Bounded by the tree,
    /// which `classify` bounded.
    pub fn rows(&self, expanded: &dyn Fn(&str) -> bool) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        self.push_children(self.root(), "", 0, expanded, &mut rows);
        rows
    }

    fn push_children(
        &self,
        id: NodeId,
        prefix: &str,
        depth: usize,
        expanded: &dyn Fn(&str) -> bool,
        rows: &mut Vec<TreeRow>,
    ) {
        let children: Vec<(String, String, NodeId)> = match &self.node(id).kind {
            NodeKind::Object(entries) => entries
                .iter()
                .map(|(key, _, child)| (key.clone(), join_key(prefix, key), *child))
                .collect(),
            NodeKind::Array(items) => items
                .iter()
                .enumerate()
                .map(|(index, child)| (format!("[{index}]"), format!("{prefix}[{index}]"), *child))
                .collect(),
            NodeKind::Scalar(_) => Vec::new(),
        };
        for (label, path, child) in children {
            let shape = match &self.node(child).kind {
                NodeKind::Object(entries) => RowShape::Container {
                    expanded: expanded(&path),
                    count: entries.len(),
                    array: false,
                },
                NodeKind::Array(items) => RowShape::Container {
                    expanded: expanded(&path),
                    count: items.len(),
                    array: true,
                },
                NodeKind::Scalar(kind) => RowShape::Scalar(kind.clone()),
            };
            let open = matches!(shape, RowShape::Container { expanded: true, .. });
            rows.push(TreeRow {
                path: path.clone(),
                depth,
                label,
                node: child,
                shape,
            });
            if open {
                self.push_children(child, &path, depth + 1, expanded, rows);
            }
        }
    }
}

/// The top-level key a path descends from: the only segment that is a real
/// column on the query side, where nested values are JSON text.
pub fn top_level_key(path: &str) -> &str {
    let end = path.find(['.', '[']).unwrap_or(path.len());
    &path[..end]
}

/// The path below its top-level key, or `None` for a top-level path.
pub fn nested_suffix(path: &str) -> Option<&str> {
    let top = top_level_key(path);
    (top.len() < path.len()).then(|| path[top.len()..].trim_start_matches('.'))
}

/// The JSONPath that addresses `path` inside its top-level column's JSON
/// text (§8.12): `http.tags[1]` → `$.tags[1]`, `a.b c` → `$['b c']`. `None`
/// for a top-level path (the column itself) or for a key the path syntax
/// cannot spell — one containing a quote or a backslash — which the caller
/// matches lexically instead.
pub fn json_path(path: &str) -> Option<String> {
    let suffix = nested_suffix(path)?;
    let mut out = String::from("$");
    for segment in split_path(suffix) {
        match segment {
            Segment::Index(index) => out.push_str(&format!("[{index}]")),
            Segment::Key(key) => {
                if key.is_empty() || key.contains(['\'', '\\']) {
                    return None;
                }
                let plain = key.chars().enumerate().all(|(index, ch)| {
                    ch == '_' || ch.is_ascii_alphabetic() || (index > 0 && ch.is_ascii_digit())
                });
                if plain {
                    out.push('.');
                    out.push_str(key);
                } else {
                    out.push_str(&format!("['{key}']"));
                }
            }
        }
    }
    Some(out)
}

enum Segment<'a> {
    Key(&'a str),
    Index(usize),
}

fn split_path(path: &str) -> Vec<Segment<'_>> {
    let mut segments = Vec::new();
    let mut rest = path;
    while !rest.is_empty() {
        if let Some(inner) = rest.strip_prefix('[') {
            let close = inner.find(']').unwrap_or(inner.len());
            if let Ok(index) = inner[..close].parse::<usize>() {
                segments.push(Segment::Index(index));
            }
            rest = inner.get(close + 1..).unwrap_or("");
            rest = rest.strip_prefix('.').unwrap_or(rest);
        } else {
            let end = rest.find(['.', '[']).unwrap_or(rest.len());
            segments.push(Segment::Key(&rest[..end]));
            rest = &rest[end..];
            rest = rest.strip_prefix('.').unwrap_or(rest);
        }
    }
    segments
}

fn join_key(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_owned()
    } else {
        format!("{prefix}.{key}")
    }
}

/// Decodes a JSON string token (quotes included) the way `classify` decodes
/// keys. `None` when the token is not a well-formed string, which `classify`
/// has already ruled out.
pub fn decode_string(token: &str) -> Option<String> {
    let inner = token.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next()? {
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            '/' => out.push('/'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'u' => {
                let mut code = 0u32;
                for _ in 0..4 {
                    code = code * 16 + chars.next()?.to_digit(16)?;
                }
                if (0xd800..=0xdbff).contains(&code) {
                    if chars.next()? != '\\' || chars.next()? != 'u' {
                        return None;
                    }
                    let mut low = 0u32;
                    for _ in 0..4 {
                        low = low * 16 + chars.next()?.to_digit(16)?;
                    }
                    code = 0x10000 + ((code - 0xd800) << 10) + low.checked_sub(0xdc00)?;
                }
                out.push(char::from_u32(code)?);
            }
            _ => return None,
        }
    }
    Some(out)
}

struct Builder<'a> {
    text: &'a str,
    spans: &'a [JsonSpan],
    at: usize,
    nodes: Vec<Node>,
}

impl Builder<'_> {
    fn peek(&self) -> Option<&JsonSpan> {
        self.spans.get(self.at)
    }

    fn punctuation_is(&self, ch: char) -> bool {
        self.peek().is_some_and(|span| {
            span.kind == JsonKind::Punctuation
                && self.text.get(span.bytes.clone()) == Some(&*ch.to_string())
        })
    }

    fn take_punctuation(&mut self, ch: char) -> Option<usize> {
        if self.punctuation_is(ch) {
            let span = &self.spans[self.at];
            self.at += 1;
            Some(span.bytes.end)
        } else {
            None
        }
    }

    fn value(&mut self) -> Option<NodeId> {
        let span = self.peek()?.clone();
        match span.kind {
            JsonKind::Punctuation if self.punctuation_is('{') => self.object(),
            JsonKind::Punctuation if self.punctuation_is('[') => self.array(),
            JsonKind::Punctuation | JsonKind::Key(_) => None,
            kind => {
                self.at += 1;
                Some(self.push(Node {
                    span: span.bytes,
                    kind: NodeKind::Scalar(kind),
                }))
            }
        }
    }

    fn push(&mut self, node: Node) -> NodeId {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    fn object(&mut self) -> Option<NodeId> {
        let start = self.peek()?.bytes.start;
        self.take_punctuation('{')?;
        // Reserve the slot so the container precedes its children, which is
        // what makes the root node 0.
        let id = self.push(Node {
            span: start..start,
            kind: NodeKind::Object(Vec::new()),
        });
        let mut entries = Vec::new();
        if let Some(end) = self.take_punctuation('}') {
            self.nodes[id].span = start..end;
            return Some(id);
        }
        loop {
            let key_span = self.peek()?.clone();
            let JsonKind::Key(key) = key_span.kind else {
                return None;
            };
            self.at += 1;
            self.take_punctuation(':')?;
            let child = self.value()?;
            entries.push((key, key_span.bytes, child));
            if self.take_punctuation(',').is_some() {
                continue;
            }
            let end = self.take_punctuation('}')?;
            self.nodes[id].span = start..end;
            self.nodes[id].kind = NodeKind::Object(entries);
            return Some(id);
        }
    }

    fn array(&mut self) -> Option<NodeId> {
        let start = self.peek()?.bytes.start;
        self.take_punctuation('[')?;
        let id = self.push(Node {
            span: start..start,
            kind: NodeKind::Array(Vec::new()),
        });
        let mut items = Vec::new();
        if let Some(end) = self.take_punctuation(']') {
            self.nodes[id].span = start..end;
            return Some(id);
        }
        loop {
            items.push(self.value()?);
            if self.take_punctuation(',').is_some() {
                continue;
            }
            let end = self.take_punctuation(']')?;
            self.nodes[id].span = start..end;
            self.nodes[id].kind = NodeKind::Array(items);
            return Some(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORD: &str = r#"{"level":"INFO","http":{"status":200,"path":"/v1","tags":["a","b"]},"empty":{},"n":null,"bad":"café 😀"}"#;

    #[test]
    fn rows_follow_document_order_and_collapse_containers_to_summaries() {
        let tree = JsonTree::parse(RECORD).unwrap();
        assert!(tree.is_object());
        let rows = tree.rows(&|_| false);
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(labels, ["level", "http", "empty", "n", "bad"]);
        assert_eq!(tree.summary(rows[1].node).as_deref(), Some("{3 keys}"));
        assert_eq!(tree.summary(rows[2].node).as_deref(), Some("{}"));
        assert_eq!(tree.scalar_text(RECORD, rows[0].node), Some("\"INFO\""));
        assert_eq!(tree.scalar_text(RECORD, rows[3].node), Some("null"));
    }

    #[test]
    fn expansion_is_by_path_and_arrays_index_their_items() {
        let tree = JsonTree::parse(RECORD).unwrap();
        let open = |path: &str| path == "http" || path == "http.tags";
        let rows = tree.rows(&open);
        let paths: Vec<&str> = rows.iter().map(|row| row.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "level",
                "http",
                "http.status",
                "http.path",
                "http.tags",
                "http.tags[0]",
                "http.tags[1]",
                "empty",
                "n",
                "bad"
            ]
        );
        assert_eq!(rows[4].depth, 1);
        assert_eq!(rows[5].depth, 2);
        assert_eq!(tree.summary(rows[4].node).as_deref(), Some("[2]"));
        assert_eq!(tree.find("http.tags[1]"), Some(rows[6].node));
        assert_eq!(tree.scalar_text(RECORD, rows[6].node), Some("\"b\""));
        assert_eq!(top_level_key("http.tags[1]"), "http");
        assert_eq!(nested_suffix("http.tags[1]"), Some("tags[1]"));
        assert_eq!(nested_suffix("level"), None);
        assert_eq!(json_path("http.tags[1]").as_deref(), Some("$.tags[1]"));
        assert_eq!(json_path("http.status").as_deref(), Some("$.status"));
        assert_eq!(json_path("a.b c.d").as_deref(), Some("$['b c'].d"));
        assert_eq!(json_path("level"), None);
        assert_eq!(json_path("a.it's"), None);
    }

    #[test]
    fn scalar_bytes_are_verbatim_and_values_decode_only_for_comparison() {
        let tree = JsonTree::parse(RECORD).unwrap();
        let bad = tree.find("bad").unwrap();
        // The screen shows the escapes exactly as the record spelled them.
        assert_eq!(tree.scalar_text(RECORD, bad), Some(r#""café 😀""#));
        assert_eq!(tree.scalar_value(RECORD, bad).as_deref(), Some("café 😀"));
        let status = tree.find("http.status").unwrap();
        assert_eq!(tree.scalar_value(RECORD, status).as_deref(), Some("200"));
    }

    #[test]
    fn anything_but_one_valid_json_value_is_no_tree() {
        assert!(JsonTree::parse("plain text").is_none());
        assert!(JsonTree::parse(r#"{"a":1} trailing"#).is_none());
        assert!(JsonTree::parse("[1, 2, [3]]").is_some_and(|tree| !tree.is_object()));
        // Lossy-decoded bytes are ordinary characters to the tree.
        let lossy = "{\"k\":\"\u{fffd}\u{fffd}\"}";
        let tree = JsonTree::parse(lossy).unwrap();
        assert_eq!(
            tree.scalar_text(lossy, tree.find("k").unwrap()),
            Some("\"\u{fffd}\u{fffd}\"")
        );
    }
}
