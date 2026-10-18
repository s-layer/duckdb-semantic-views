//! The parse tree a grammar transform receives from the C++ bridge.
//!
//! `DuckDB`'s PEG parser hands a transform the `ParseResult` subtree its rule
//! matched. The C++ side (`sv_grammar_serialise` in `cpp/src/shim.cpp`, called
//! from `sv_grammar_lower`) serialises that subtree and the statement's tokens
//! into one buffer; this module decodes it. The wire format, little-endian throughout:
//!
//! ```text
//! buffer := node token_count:u32 token* source
//! node   := kind:u8 name_len:u32 name offset:u32 length:u32
//!           value_len:u32 value child_count:u32 node*
//! token  := offset:u32 length:u32 newline:u8 text_len:u32 text
//! source := source_offset:u32 source_len:u32 source
//! ```
//!
//! The tokens are the statement's own: those inside the matched node's span,
//! not every token of the query around it, so a string of K statements costs
//! each one only its own length (TECH-DEBT #85).
//!
//! `offset` / `length` are byte positions in the query text, `u32::MAX` when
//! the node has none. `value` is the leaf's decoded value (an identifier
//! without its quotes, a string literal's contents) and is empty for
//! structural nodes. `newline` is 1 when a line break comes between the token
//! and the one before it. `source` is the statement's source text, which
//! starts at query offset `source_offset`, when the bridge knows it, and empty
//! otherwise: `DuckDB` gives a transform only the tokens, so the bridge passes
//! the statement's part of the query the parser override saw on this thread,
//! and only when every token of the statement is that query's text at the
//! token's offset. Without it the text is rebuilt from the tokens
//! ([`Tree::tokens_in`], [`Tree::text`]).

/// The `ParseResultType` of a node, as `DuckDB` numbers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    List,
    Optional,
    Repeat,
    Choice,
    Identifier,
    Keyword,
    Operator,
    Number,
    String,
    Token,
    /// A type this bridge does not interpret (end of input, or one added by a
    /// later `DuckDB`).
    Other,
}

impl Kind {
    fn from_wire(byte: u8) -> Self {
        // Mirrors `enum class ParseResultType` in DuckDB's parse_result.hpp.
        match byte {
            0 => Self::List,
            1 => Self::Optional,
            2 => Self::Repeat,
            3 => Self::Choice,
            5 => Self::Identifier,
            6 => Self::Keyword,
            7 => Self::Operator,
            10 => Self::Number,
            11 => Self::String,
            13 => Self::Token,
            _ => Self::Other,
        }
    }
}

/// One node of the matched subtree.
#[derive(Debug, Clone)]
pub(crate) struct Node {
    /// The grammar rule that produced it, empty for anonymous nodes.
    pub(crate) rule: String,
    pub(crate) kind: Kind,
    /// `(offset, length)` in bytes of the query text.
    pub(crate) span: Option<(usize, usize)>,
    /// A leaf's decoded value; empty for structural nodes.
    pub(crate) value: String,
    pub(crate) children: Vec<Node>,
}

/// One token of the statement, as the tokenizer saw it.
#[derive(Debug, Clone)]
pub(crate) struct Token {
    pub(crate) offset: usize,
    pub(crate) len: usize,
    pub(crate) text: String,
    /// A line break comes between this token and the one before it.
    pub(crate) newline: bool,
}

/// A decoded subtree with the statement's tokens.
#[derive(Debug, Clone)]
pub(crate) struct Tree {
    pub(crate) root: Node,
    pub(crate) tokens: Vec<Token>,
    /// The statement's source text, when the bridge has it (see the module
    /// doc).
    pub(crate) source: Option<Source>,
}

/// Source text that starts at byte `offset` of the query.
#[derive(Debug, Clone)]
pub(crate) struct Source {
    pub(crate) offset: usize,
    pub(crate) text: String,
}

impl Node {
    /// The first node named `rule` in pre-order, this node included. The
    /// search does not descend into a match.
    pub(crate) fn find(&self, rule: &str) -> Option<&Node> {
        if self.rule.eq_ignore_ascii_case(rule) {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(rule))
    }

    /// Every node named `rule` in pre-order, not descending into a match.
    pub(crate) fn find_all<'a>(&'a self, rule: &str) -> Vec<&'a Node> {
        let mut out = Vec::new();
        self.collect(rule, &mut out);
        out
    }

    fn collect<'a>(&'a self, rule: &str, out: &mut Vec<&'a Node>) {
        if self.rule.eq_ignore_ascii_case(rule) {
            out.push(self);
            return;
        }
        for c in &self.children {
            c.collect(rule, out);
        }
    }

    /// The first leaf of `kind` under this node, in pre-order.
    pub(crate) fn first_leaf(&self, kind: Kind) -> Option<&Node> {
        if self.kind == kind {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.first_leaf(kind))
    }

    /// The byte offset this node starts at, if it has one.
    pub(crate) fn offset(&self) -> Option<usize> {
        self.span.map(|(o, _)| o)
    }
}

impl Tree {
    /// The tokens that lie wholly inside `node`'s span, in order.
    pub(crate) fn tokens_in<'a>(&'a self, node: &Node) -> impl Iterator<Item = &'a Token> {
        let (start, end) = node.span.map_or((1, 0), |(o, l)| (o, o + l));
        self.tokens
            .iter()
            .filter(move |t| t.offset >= start && t.offset + t.len <= end)
    }

    /// `node`'s text, which starts at query offset `node.offset()`. With the
    /// statement's source text (`source`) it is the node's source text.
    /// Without it the text is rebuilt from the tokens, each at its offset, and
    /// spaces fill the gaps between them, which held whitespace and comments; a
    /// gap that held a line break starts with one (the tokens record only that
    /// there was one), so tabs, comments and blank lines are lost (TECH-DEBT
    /// #100).
    pub(crate) fn text(&self, node: &Node) -> String {
        let Some((start, len)) = node.span else {
            return String::new();
        };
        if let Some(source) = &self.source {
            let text = start
                .checked_sub(source.offset)
                .and_then(|at| source.text.get(at..at + len));
            if let Some(text) = text {
                return text.to_string();
            }
        }
        let mut out = String::new();
        for t in self.tokens_in(node) {
            let at = start + out.len();
            if t.offset > at {
                let gap = t.offset - at;
                if t.newline {
                    out.push('\n');
                    out.extend(std::iter::repeat_n(' ', gap - 1));
                } else {
                    out.extend(std::iter::repeat_n(' ', gap));
                }
            }
            out.push_str(&t.text);
        }
        out
    }

    /// Decode the buffer `sv_grammar_serialise` wrote.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, String> {
        let mut r = Reader { bytes, pos: 0 };
        let root = r.node(0)?;
        let count = r.u32()? as usize;
        let mut tokens = Vec::with_capacity(count.min(4096));
        for _ in 0..count {
            let offset = r.u32()? as usize;
            let len = r.u32()? as usize;
            let newline = r.u8()? != 0;
            let text = r.string()?;
            tokens.push(Token {
                offset,
                len,
                text,
                newline,
            });
        }
        let source_offset = r.u32()? as usize;
        let source = Some(r.string()?)
            .filter(|s| !s.is_empty())
            .map(|text| Source {
                offset: source_offset,
                text,
            });
        if r.pos != bytes.len() {
            return Err(format!(
                "{} trailing bytes after the parse tree",
                bytes.len() - r.pos
            ));
        }
        Ok(Self {
            root,
            tokens,
            source,
        })
    }
}

/// The deepest tree the decoder accepts. `DuckDB` bounds its own parse depth;
/// this only keeps a corrupt buffer from recursing without limit.
const MAX_DEPTH: usize = 4096;

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.bytes.len())
            .ok_or_else(|| format!("parse tree truncated at byte {}", self.pos))?;
        let out = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn string(&mut self) -> Result<String, String> {
        let len = self.u32()? as usize;
        let b = self.take(len)?;
        String::from_utf8(b.to_vec()).map_err(|e| format!("parse tree text is not UTF-8: {e}"))
    }

    fn node(&mut self, depth: usize) -> Result<Node, String> {
        if depth > MAX_DEPTH {
            return Err("parse tree nested too deeply".to_string());
        }
        let kind = Kind::from_wire(self.u8()?);
        let rule = self.string()?;
        let offset = self.u32()?;
        let length = self.u32()?;
        let span = (offset != u32::MAX).then(|| {
            let len = if length == u32::MAX {
                0
            } else {
                length as usize
            };
            (offset as usize, len)
        });
        let value = self.string()?;
        let count = self.u32()? as usize;
        let mut children = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            children.push(self.node(depth + 1)?);
        }
        Ok(Node {
            rule,
            kind,
            span,
            value,
            children,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{Kind, Node, Source, Token, Tree};

    /// Encode a tree in the bridge's wire format, for tests. `source` is the
    /// source text and the query offset it starts at.
    pub(crate) fn encode(
        root: &Node,
        tokens: &[(usize, &str, bool)],
        source: (usize, &str),
    ) -> Vec<u8> {
        fn put_str(out: &mut Vec<u8>, s: &str) {
            out.extend_from_slice(&u32::try_from(s.len()).unwrap().to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        fn put_node(out: &mut Vec<u8>, n: &Node) {
            let kind = match n.kind {
                Kind::List => 0,
                Kind::Optional => 1,
                Kind::Repeat => 2,
                Kind::Choice => 3,
                Kind::Identifier => 5,
                Kind::Keyword => 6,
                Kind::Operator => 7,
                Kind::Number => 10,
                Kind::String => 11,
                Kind::Token => 13,
                Kind::Other => 12,
            };
            out.push(kind);
            put_str(out, &n.rule);
            let (o, l) = n.span.map_or((u32::MAX, u32::MAX), |(o, l)| {
                (u32::try_from(o).unwrap(), u32::try_from(l).unwrap())
            });
            out.extend_from_slice(&o.to_le_bytes());
            out.extend_from_slice(&l.to_le_bytes());
            put_str(out, &n.value);
            out.extend_from_slice(&u32::try_from(n.children.len()).unwrap().to_le_bytes());
            for c in &n.children {
                put_node(out, c);
            }
        }
        let mut out = Vec::new();
        put_node(&mut out, root);
        out.extend_from_slice(&u32::try_from(tokens.len()).unwrap().to_le_bytes());
        for (offset, text, newline) in tokens {
            out.extend_from_slice(&u32::try_from(*offset).unwrap().to_le_bytes());
            out.extend_from_slice(&u32::try_from(text.len()).unwrap().to_le_bytes());
            out.push(u8::from(*newline));
            put_str(&mut out, text);
        }
        out.extend_from_slice(&u32::try_from(source.0).unwrap().to_le_bytes());
        put_str(&mut out, source.1);
        out
    }

    pub(crate) fn leaf(rule: &str, kind: Kind, offset: usize, len: usize, value: &str) -> Node {
        Node {
            rule: rule.to_string(),
            kind,
            span: Some((offset, len)),
            value: value.to_string(),
            children: Vec::new(),
        }
    }

    pub(crate) fn list(rule: &str, offset: usize, len: usize, children: Vec<Node>) -> Node {
        Node {
            rule: rule.to_string(),
            kind: Kind::List,
            span: Some((offset, len)),
            value: String::new(),
            children,
        }
    }

    #[test]
    fn decode_round_trips_a_tree() {
        let root = list(
            "SvRoot",
            0,
            9,
            vec![
                leaf("", Kind::Keyword, 0, 4, "SHOW"),
                leaf("Name", Kind::Identifier, 5, 4, "a b"),
            ],
        );
        let bytes = encode(&root, &[(0, "SHOW", false), (5, "\"a b\"", true)], (0, ""));
        let tree = Tree::decode(&bytes).unwrap();
        assert_eq!(tree.root.rule, "SvRoot");
        assert_eq!(tree.root.children.len(), 2);
        assert_eq!(tree.root.find("name").unwrap().value, "a b");
        assert_eq!(tree.tokens.len(), 2);
        assert!(!tree.tokens[0].newline);
        assert!(tree.tokens[1].newline);
        assert!(tree.source.is_none());
        let bytes = encode(&root, &[], (3, "SHOW\n\"a b\""));
        let source = Tree::decode(&bytes).unwrap().source.unwrap();
        assert_eq!(source.offset, 3);
        assert_eq!(source.text, "SHOW\n\"a b\"");
    }

    #[test]
    fn tokens_in_keeps_only_the_tokens_inside_the_span() {
        let root = list("R", 2, 5, vec![]);
        let tok = |offset: usize, text: &str| super::Token {
            offset,
            len: text.len(),
            text: text.into(),
            newline: false,
        };
        let tree = Tree {
            root,
            tokens: vec![
                tok(0, "a"),
                tok(2, "b"),
                tok(4, "cd"),
                tok(6, "ef"),
                tok(9, "g"),
            ],
            source: None,
        };
        let texts: Vec<_> = tree
            .tokens_in(&tree.root)
            .map(|t| t.text.as_str())
            .collect();
        assert_eq!(texts, ["b", "cd"]);
    }

    #[test]
    fn text_keeps_every_token_at_its_offset_from_the_node() {
        // `SELECT 1; CREATE x -- note\n  AS  y`. The tokenizer drops the
        // comment; the node is the CREATE statement.
        let tok = |offset: usize, text: &str, newline: bool| Token {
            offset,
            len: text.len(),
            text: text.into(),
            newline,
        };
        let tree = Tree {
            root: list("R", 10, 24, vec![]),
            tokens: vec![
                tok(0, "SELECT", false),
                tok(7, "1", false),
                tok(8, ";", false),
                tok(10, "CREATE", false),
                tok(17, "x", false),
                tok(29, "AS", true),
                tok(33, "y", false),
            ],
            source: None,
        };
        let text = tree.text(&tree.root);
        assert_eq!(text, format!("CREATE x\n{}AS  y", " ".repeat(10)));
        for t in tree.tokens_in(&tree.root) {
            assert_eq!(&text[t.offset - 10..t.offset - 10 + t.len], t.text);
        }
    }

    /// With the statement's source text the node's text is its source, tabs,
    /// comments and blank lines included, for the statement and for a node
    /// inside it.
    #[test]
    fn text_takes_the_source_when_the_bridge_has_it() {
        let sql = "SELECT 1; CREATE x -- note\n\n\tAS  y";
        let inner = list("X", 17, sql.len() - 17, vec![]);
        let mut tree = super::tests::tree(sql, list("R", 10, sql.len() - 10, vec![inner]));
        tree.source = Some(Source {
            offset: 10,
            text: sql[10..].to_string(),
        });
        assert_eq!(tree.text(&tree.root), "CREATE x -- note\n\n\tAS  y");
        assert_eq!(
            tree.text(tree.root.find("X").unwrap()),
            "x -- note\n\n\tAS  y"
        );
    }

    #[test]
    fn decode_rejects_a_truncated_buffer() {
        let root = list("R", 0, 1, vec![]);
        let mut bytes = encode(&root, &[], (0, ""));
        bytes.pop();
        assert!(Tree::decode(&bytes).is_err());
    }

    #[test]
    fn decode_rejects_trailing_bytes() {
        let root = list("R", 0, 1, vec![]);
        let mut bytes = encode(&root, &[], (0, ""));
        bytes.push(0);
        assert!(Tree::decode(&bytes).unwrap_err().contains("trailing"));
    }

    #[test]
    fn find_does_not_descend_into_a_match() {
        let inner = list("X", 2, 1, vec![]);
        let outer = list("X", 0, 5, vec![inner]);
        let root = list("R", 0, 5, vec![outer]);
        assert_eq!(root.find_all("X").len(), 1);
        assert_eq!(root.find("x").unwrap().span, Some((0, 5)));
    }

    /// A node spanning the first occurrence of `text` in `sql`.
    pub(crate) fn n(
        sql: &str,
        rule: &str,
        text: &str,
        kind: Kind,
        value: &str,
        children: Vec<Node>,
    ) -> Node {
        let offset = sql
            .find(text)
            .unwrap_or_else(|| panic!("{text} not in {sql}"));
        Node {
            rule: rule.to_string(),
            kind,
            span: Some((offset, text.len())),
            value: value.to_string(),
            children,
        }
    }

    /// The statement's tokens: words split on whitespace, with `.` split off
    /// as its own token.
    pub(crate) fn tree(sql: &str, root: Node) -> Tree {
        let mut tokens = Vec::new();
        let mut start = None;
        for (i, c) in sql.char_indices().chain(std::iter::once((sql.len(), ' '))) {
            let boundary = c.is_whitespace() || c == '.';
            if boundary {
                if let Some(s) = start.take() {
                    tokens.push(Token {
                        offset: s,
                        len: i - s,
                        text: sql[s..i].to_string(),
                        newline: sql[..s].trim_end_matches([' ', '\t']).ends_with('\n'),
                    });
                }
                if c == '.' {
                    tokens.push(Token {
                        offset: i,
                        len: 1,
                        text: ".".to_string(),
                        newline: false,
                    });
                }
            } else if start.is_none() {
                start = Some(i);
            }
        }
        Tree {
            root,
            tokens,
            source: None,
        }
    }

    pub(crate) fn root(sql: &str, rule: &str, children: Vec<Node>) -> Node {
        n(sql, rule, sql, Kind::List, "", children)
    }
}
