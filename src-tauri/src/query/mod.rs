pub mod ast;
pub mod intent;
pub mod lexer;
pub mod parser;
pub mod router;
pub mod scope_trie;

pub use ast::{ActiveFilterDto, FileKind, Filter, Mode, Query, RangeOp, Scope, Term, TermField};
pub use intent::{Intent, InterpretationChip, compile};
pub use parser::parse_query;
pub use router::{QueryRoute, route_query};
pub use scope_trie::{DirectoryTrie, ScopeResolution, TrieMatchResult};
