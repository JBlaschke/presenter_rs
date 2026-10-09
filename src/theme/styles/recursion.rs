//! Recursion in SCSS stylesheets (module docs of [`super`], "Isolation"): a lenient reading
//! of the SCSS syntax that finds the `@mixin`s that include themselves and the
//! `@function`s that call themselves, directly or through others of the same file.
//!
//! grass has no recursion limit: a recursion that never ends overflows the stack of the
//! thread compiling, which aborts the process. In the presenter binary that is a worker
//! process, and [`recursion_hint`] explains its crash; a compilation without a worker
//! (the library on its own) rejects a file with [`always_recursion`] before grass runs it.
//!
//! A definition counts only when something may use it ("reached"): a reference outside
//! definitions in its own file, any reference in another file of the compilation, or a
//! reference in a reached definition. A reference is a `@include name` or a call
//! `name(…)`; a namespaced one (`@include m.pad`, `m.f(1)`) names a member of another
//! module, so it reaches definitions of other files only.
//!
//! A reference is **always** taken when nothing may skip it: for a mixin, an `@include`
//! outside control directives (`@if`, `@else`, `@elseif`, `@each`, `@for`, `@while`) and
//! content blocks; for a function, a call before any `@return` (an `@if` that may return
//! makes what follows conditional), outside control directives, the lazy `if()` and the
//! right-hand side of `and`/`or` (which Sass evaluates only when needed).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::iter::Peekable;
use std::str::Chars;

/// What kind of definition recurses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RecursionKind {
    /// A `@mixin` that includes itself.
    Mixin,
    /// A `@function` that calls itself.
    Function,
}

/// A `@mixin` or `@function` that recurses ([`unconditional_recursion`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recursion {
    /// A mixin or a function.
    pub kind: RecursionKind,
    /// The names on the cycle, starting and ending with the one defined first (`[a, a]`
    /// for a mixin that includes itself).
    pub cycle: Vec<String>,
    /// The 1-based line of the `@include` or call in the first definition that starts the
    /// cycle.
    pub line: usize,
    /// True if the recursion is always taken; false if a condition may skip it (one that
    /// never does recurses until the stack overflows, too).
    pub always: bool,
}

impl Recursion {
    /// The message of the [`crate::error::Error::Style`] for this recursion in the file
    /// `text`: `line L: …` and the line itself, numbered (as grass quotes a line).
    pub fn message(&self, text: &str) -> String {
        let (name, calls, call) = match self.kind {
            RecursionKind::Mixin => ("@mixin", "includes itself", "includes itself"),
            RecursionKind::Function => (
                "@function",
                "calls itself before any @return",
                "calls itself",
            ),
        };
        let through = match self.cycle.as_slice() {
            [_, _] => String::new(),
            cycle => format!(" through {}", cycle[1..cycle.len() - 1].join(", ")),
        };
        let first = &self.cycle[0];
        let mut message = match self.always {
            true => format!(
                "line {}: {name} {first} always {calls}{through}, so Sass recurses until it \
                 runs out of stack; make the recursion depend on an @if that ends it",
                self.line
            ),
            false => format!(
                "line {}: Sass ran out of stack, most likely in the recursion of {name} \
                 {first}, which {call}{through} here: the condition meant to end it is \
                 never met",
                self.line
            ),
        };
        if let Some(source) = text.lines().nth(self.line - 1) {
            let _ = write!(message, "\n {} | {}", self.line, source.trim_end());
        }
        message
    }

    /// This recursion in a list of candidates ([`Hint::Candidates`]), in the file `file`:
    /// `@function f (<file>:<line>)`, or `@mixin a (<file>:<line>, through b)`.
    pub(super) fn summary(&self, file: &str) -> String {
        let name = match self.kind {
            RecursionKind::Mixin => "@mixin",
            RecursionKind::Function => "@function",
        };
        let through = match self.cycle.as_slice() {
            [_, _] => String::new(),
            cycle => format!(", through {}", cycle[1..cycle.len() - 1].join(", ")),
        };
        format!("{name} {} ({file}:{}{through})", self.cycle[0], self.line)
    }
}

/// The names a file refers to, by kind ([`always_recursion`]).
pub(super) type Refs = BTreeSet<(RecursionKind, String)>;

/// The first reached `@mixin` (in source order) of the SCSS file `scss` that always
/// includes itself, else the first such `@function` that always calls itself (module
/// docs); `None` when there is neither. Only references in `scss` itself reach its
/// definitions. Invalid input finds nothing rather than failing.
pub fn unconditional_recursion(scss: &str) -> Option<Recursion> {
    always_recursion(scss, &Refs::new()).0
}

/// [`unconditional_recursion`] in `scss`, one file of a compilation, whose definitions the
/// references of the files loaded before (`elsewhere`) reach too; and every reference of
/// `scss`, for the files loaded after it.
pub(super) fn always_recursion(scss: &str, elsewhere: &Refs) -> (Option<Recursion>, Refs) {
    let scan = FileScan::of(scss);
    let found = scan.cycle(elsewhere, true);
    (found, scan.all_refs())
}

/// What [`recursion_hint`] finds in the SCSS files of a compilation that overflowed its
/// stack, each recursion with the index of its file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Hint {
    /// The recursion that most likely overflowed the stack: the first reached definition
    /// that always recurses, else the only reached one that recurses at all.
    One(usize, Recursion),
    /// Two or more reached recursions that a condition guards, in the order of the files
    /// (in a file: mixins, then functions, in source order), one per cycle: any of them
    /// may be the one whose condition is never met, so none is blamed (a correct recursive
    /// helper, such as a `str-replace` that recurses inside an `@if`, is one of them).
    Candidates(Vec<(usize, Recursion)>),
}

/// The recursion most likely to have overflowed the stack of a compilation of `files`
/// (its SCSS files, `(name, text)` in the order loaded; [`Hint`]): the first reached
/// definition that always recurses (in the order of `files`), else the reached
/// definitions that recurse at all, named only when there is one.
pub(super) fn recursion_hint(files: &[(String, String)]) -> Option<Hint> {
    let scans: Vec<FileScan> = files.iter().map(|(_, text)| FileScan::of(text)).collect();
    let elsewhere = |at: usize| -> Refs {
        let others = scans.iter().enumerate().filter(|(other, _)| *other != at);
        others.flat_map(|(_, scan)| scan.all_refs()).collect()
    };
    let always = scans
        .iter()
        .enumerate()
        .find_map(|(at, scan)| Some((at, scan.cycle(&elsewhere(at), true)?)));
    if let Some((at, found)) = always {
        return Some(Hint::One(at, found));
    }
    let mut found: Vec<(usize, Recursion)> = Vec::new();
    for (at, scan) in scans.iter().enumerate() {
        let reached = scan.reached(&elsewhere(at));
        for kind in [RecursionKind::Mixin, RecursionKind::Function] {
            let cycles = scan.cycles_of(kind, &reached, false, usize::MAX);
            found.extend(cycles.into_iter().map(|cycle| (at, cycle)));
        }
    }
    match found.len() {
        0 => None,
        1 => found.pop().map(|(at, recursion)| Hint::One(at, recursion)),
        _ => Some(Hint::Candidates(found)),
    }
}

/// A reference to a mixin (`@include`) or a function (a call).
#[derive(Debug, Clone)]
struct Ref {
    kind: RecursionKind,
    /// The name, normalized ([`sass_name`]); for a namespaced reference, the member's.
    name: String,
    /// The 1-based line of the statement.
    line: usize,
    /// True if it is always taken (module docs).
    always: bool,
    /// True if it is namespaced (another module's member).
    foreign: bool,
}

/// A `@mixin` or `@function` definition and its references.
#[derive(Debug)]
struct Definition {
    kind: RecursionKind,
    name: String,
    refs: Vec<Ref>,
}

/// The definitions of one file and its references outside them.
#[derive(Debug, Default)]
struct FileScan {
    definitions: Vec<Definition>,
    top: Vec<Ref>,
}

impl FileScan {
    fn of(scss: &str) -> FileScan {
        let mut scan = FileScan::default();
        scan.collect(&parse_statements(scss));
        scan
    }

    /// Collects the definitions of `statements` (at any depth outside definitions) and the
    /// references outside them.
    fn collect(&mut self, statements: &[Statement]) {
        for statement in statements {
            let at = statement.at_rule();
            let kind = match at.as_str() {
                "mixin" => Some(RecursionKind::Mixin),
                "function" => Some(RecursionKind::Function),
                _ => None,
            };
            let name = leading_name(statement.at_rule_rest());
            if let (Some(kind), Some(name), Some(block)) = (kind, name, &statement.block) {
                let mut refs = Vec::new();
                match kind {
                    RecursionKind::Mixin => mixin_refs(block, true, &mut refs),
                    RecursionKind::Function => function_refs(block, true, &mut refs),
                }
                self.definitions.push(Definition { kind, name, refs });
                continue;
            }
            statement_refs(statement, false, &mut self.top);
            if let Some(block) = &statement.block {
                self.collect(block);
            }
        }
    }

    /// Every name the file refers to, inside definitions or not.
    fn all_refs(&self) -> Refs {
        let inside = self.definitions.iter().flat_map(|def| def.refs.iter());
        let refs = self.top.iter().chain(inside);
        refs.map(|r| (r.kind, r.name.clone())).collect()
    }

    /// The definitions reached (module docs) from the file's own references outside
    /// definitions and from `elsewhere`.
    fn reached(&self, elsewhere: &Refs) -> Refs {
        let own = self.top.iter().filter(|r| !r.foreign);
        let mut reached: Refs = own.map(|r| (r.kind, r.name.clone())).collect();
        reached.extend(elsewhere.iter().cloned());
        let mut todo: Vec<(RecursionKind, String)> = reached.iter().cloned().collect();
        while let Some((kind, name)) = todo.pop() {
            let defs = self.definitions.iter();
            let def = defs.filter(|def| def.kind == kind && def.name == name);
            for r in def.flat_map(|def| def.refs.iter()).filter(|r| !r.foreign) {
                if reached.insert((r.kind, r.name.clone())) {
                    todo.push((r.kind, r.name.clone()));
                }
            }
        }
        reached
    }

    /// The first reached mixin, else function (in source order), that reaches itself along
    /// its references (only the always-taken ones if `always`).
    fn cycle(&self, elsewhere: &Refs, always: bool) -> Option<Recursion> {
        let reached = self.reached(elsewhere);
        [RecursionKind::Mixin, RecursionKind::Function]
            .into_iter()
            .find_map(|kind| self.cycle_of(kind, &reached, always))
    }

    /// [`FileScan::cycle`] among the `reached` definitions of `kind`.
    fn cycle_of(&self, kind: RecursionKind, reached: &Refs, always: bool) -> Option<Recursion> {
        self.cycles_of(kind, reached, always, 1).pop()
    }

    /// Up to `limit` cycles among the `reached` definitions of `kind`, in source order of
    /// the definitions that start them; a definition on a cycle found before starts none.
    fn cycles_of(
        &self,
        kind: RecursionKind,
        reached: &Refs,
        always: bool,
        limit: usize,
    ) -> Vec<Recursion> {
        let defs: Vec<&Definition> = self.definitions.iter().filter(|d| d.kind == kind).collect();
        // The first definition of a name counts.
        let edges: BTreeMap<&str, Vec<&Ref>> = defs
            .iter()
            .rev()
            .map(|def| (def.name.as_str(), edges_of(def, always)))
            .collect();
        let reached = defs
            .iter()
            .filter(|def| reached.contains(&(kind, def.name.clone())));
        let mut found: Vec<Recursion> = Vec::new();
        let mut on_a_cycle: BTreeSet<String> = BTreeSet::new();
        for def in reached {
            if found.len() >= limit {
                break;
            }
            if !on_a_cycle.insert(def.name.clone()) {
                continue;
            }
            let Some(next) = edges.get(def.name.as_str()) else {
                continue;
            };
            let cycle = next.iter().find_map(|next| {
                let mut path = vec![def.name.clone()];
                let found = cycle_back(
                    &def.name,
                    &next.name,
                    &edges,
                    &mut path,
                    &mut BTreeSet::new(),
                );
                found.then_some(Recursion {
                    kind,
                    cycle: path,
                    line: next.line,
                    always,
                })
            });
            if let Some(cycle) = cycle {
                on_a_cycle.extend(cycle.cycle.iter().cloned());
                found.push(cycle);
            }
        }
        found
    }
}

/// The references of `def` to definitions of its own kind in its own file (only the
/// always-taken ones if `always`).
fn edges_of(def: &Definition, always: bool) -> Vec<&Ref> {
    let refs = def.refs.iter();
    refs.filter(|r| r.kind == def.kind && !r.foreign && (r.always || !always))
        .collect()
}

/// True if `start` is reached from `name` along `edges`; `path` then ends with the names
/// from `name` to `start`. `seen` holds the names already explored.
fn cycle_back(
    start: &str,
    name: &str,
    edges: &BTreeMap<&str, Vec<&Ref>>,
    path: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
) -> bool {
    path.push(name.to_string());
    if name == start {
        return true;
    }
    if seen.insert(name.to_string())
        && let Some(next) = edges.get(name)
        && next
            .iter()
            .any(|r| cycle_back(start, &r.name, edges, path, seen))
    {
        return true;
    }
    path.pop();
    false
}

/// The control directives: what is inside them may not run.
const CONTROL: [&str; 6] = ["if", "else", "elseif", "each", "for", "while"];

/// The references of a mixin's `block`; `always` while nothing may skip it.
fn mixin_refs(block: &[Statement], always: bool, refs: &mut Vec<Ref>) {
    for statement in block {
        let at = statement.at_rule();
        let control = CONTROL.contains(&at.as_str());
        if matches!(at.as_str(), "mixin" | "function") {
            continue; // not allowed inside a mixin
        }
        statement_refs(statement, always && !control, refs);
        if let Some(inner) = &statement.block {
            // A content block runs when (and if) the included mixin says `@content`.
            let taken = always && !control && at != "include";
            mixin_refs(inner, taken, refs);
        }
    }
}

/// The references of a function's `block` up to its first `@return` or `@error` (nothing
/// after it runs); `always` while nothing may skip them.
fn function_refs(block: &[Statement], mut always: bool, refs: &mut Vec<Ref>) {
    for statement in block {
        let at = statement.at_rule();
        if CONTROL.contains(&at.as_str()) {
            statement_refs(statement, false, refs);
            if let Some(inner) = &statement.block {
                function_refs(inner, false, refs);
                if returns(inner) {
                    always = false; // what follows runs only if that branch did not return
                }
            }
            continue;
        }
        statement_refs(statement, always, refs);
        if matches!(at.as_str(), "return" | "error") {
            return;
        }
    }
}

/// True if `block` contains a `@return` or `@error` at any depth.
fn returns(block: &[Statement]) -> bool {
    block.iter().any(|statement| {
        matches!(statement.at_rule().as_str(), "return" | "error")
            || statement.block.as_deref().is_some_and(returns)
    })
}

/// The references in `statement`'s own text: an `@include` and the calls in its
/// arguments, or the calls of any other statement (a declaration, `@return`, a condition,
/// a selector's harmless `not(`).
fn statement_refs(statement: &Statement, always: bool, refs: &mut Vec<Ref>) {
    let at = statement.at_rule();
    let line = statement.line;
    let mut expression = match at.is_empty() {
        true => statement.head.as_str(),
        false => statement.at_rule_rest(),
    };
    if at == "include"
        && let Some(len) = identifier_len(expression)
    {
        let (mut name, mut foreign) = (&expression[..len], false);
        let mut rest = &expression[len..];
        if let Some(member) = rest.strip_prefix('.')
            && let Some(member_len) = identifier_len(member)
        {
            (name, foreign, rest) = (&member[..member_len], true, &member[member_len..]);
        }
        refs.push(Ref {
            kind: RecursionKind::Mixin,
            name: sass_name(name),
            line,
            always,
            foreign,
        });
        expression = rest;
    }
    refs.extend(
        called_names(expression, always)
            .into_iter()
            .map(|call| Ref { line, ..call }),
    );
}

/// The functions `expression` calls (`name(`; a namespaced `m.name(` is foreign), each
/// always taken if `always` and it is not inside the lazy `if()` or after an `and`/`or`
/// of its parentheses (or an enclosing pair). Lines are left 0.
fn called_names(expression: &str, always: bool) -> Vec<Ref> {
    let mut calls = Vec::new();
    let bytes = expression.as_bytes();
    let (mut depth, mut lazy_from) = (0usize, None::<usize>);
    let mut at = 0;
    while at < expression.len() {
        let rest = &expression[at..];
        let Some(len) = identifier_len(rest) else {
            match bytes[at] {
                b'(' => depth += 1,
                b')' => {
                    depth = depth.saturating_sub(1);
                    lazy_from = lazy_from.filter(|&from| from <= depth);
                }
                _ => {}
            }
            at += rest.chars().next().map_or(1, char::len_utf8);
            continue;
        };
        let word = &rest[..len];
        let before = expression[..at].chars().next_back();
        let starts_word = !matches!(before, Some('$' | '@' | '%' | '-' | '_'))
            && !before.is_some_and(char::is_alphanumeric);
        at += len;
        if bytes.get(at) != Some(&b'(') || !starts_word {
            if starts_word && matches!(word, "and" | "or") {
                lazy_from = Some(lazy_from.map_or(depth, |from| from.min(depth)));
            }
            continue;
        }
        let foreign = before == Some('.');
        let name = sass_name(word);
        if name == "if" && !foreign {
            // Its arguments (one level deeper) are evaluated when needed.
            lazy_from = Some(lazy_from.map_or(depth + 1, |from| from.min(depth + 1)));
            continue;
        }
        calls.push(Ref {
            kind: RecursionKind::Function,
            name,
            line: 0,
            always: always && lazy_from.is_none(),
            foreign,
        });
    }
    calls
}

/// The byte length of the identifier (letters, digits, `-`, `_`) at the start of `text`.
fn identifier_len(text: &str) -> Option<usize> {
    let end = text
        .find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(text.len());
    (end > 0).then_some(end)
}

/// A name as Sass compares it: `-` and `_` are the same.
fn sass_name(name: &str) -> String {
    name.replace('_', "-")
}

/// The leading identifier of `text`, normalized.
fn leading_name(text: &str) -> Option<String> {
    identifier_len(text).map(|len| sass_name(&text[..len]))
}

/// One SCSS statement: its text up to `;` or `{` (comments removed, strings emptied) and
/// its block, if it has one.
#[derive(Debug, Default)]
struct Statement {
    /// The statement's text, trimmed.
    head: String,
    /// The 1-based line where it starts.
    line: usize,
    /// The statements of its `{ … }` block.
    block: Option<Vec<Statement>>,
}

impl Statement {
    /// The at-rule's name (`include` for `@include pad;`), lowercase; `""` for anything
    /// else.
    fn at_rule(&self) -> String {
        let Some(rest) = self.head.strip_prefix('@') else {
            return String::new();
        };
        rest.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    }

    /// The text after the at-rule's name.
    fn at_rule_rest(&self) -> &str {
        let name = 1 + self.at_rule().len();
        self.head.get(name..).unwrap_or_default().trim_start()
    }
}

/// The SCSS statements of `text`, as a tree (see [`Statement`]), in one pass.
fn parse_statements(text: &str) -> Vec<Statement> {
    let mut stack = vec![Statement::default()];
    let mut head = Statement::default();
    let mut chars = text.chars().peekable();
    let mut line = 1;
    while let Some(c) = chars.next() {
        let comment = c == '/' && matches!(chars.peek(), Some('/' | '*'));
        if !comment
            && !c.is_whitespace()
            && !matches!(c, '{' | ';' | '}')
            && head.head.trim().is_empty()
        {
            head.line = line;
            head.head.clear();
        }
        match c {
            '/' if chars.peek() == Some(&'/') => while chars.next_if(|&c| c != '\n').is_some() {},
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut last = ' ';
                for c in chars.by_ref() {
                    line += usize::from(c == '\n');
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
            }
            '"' | '\'' => {
                line += skip_string(&mut chars, c);
                head.head.push_str("\"\"");
            }
            '(' if ends_with_url(&head.head) => {
                head.head.push('(');
                line += skip_unquoted_url(&mut chars, &mut head.head);
            }
            '#' if chars.peek() == Some(&'{') => {
                head.head.push('#');
                line += copy_interpolation(&mut chars, &mut head.head);
            }
            '{' | ';' | '}' => {
                let done = std::mem::take(&mut head);
                if c == '{' {
                    stack.push(done);
                    continue;
                }
                if !done.head.trim().is_empty()
                    && let Some(open) = stack.last_mut()
                {
                    open.block.get_or_insert_default().push(trimmed(done));
                }
                if c == '}' && stack.len() > 1 {
                    close_block(&mut stack);
                }
            }
            _ => {
                line += usize::from(c == '\n');
                head.head.push(c);
            }
        }
    }
    while stack.len() > 1 {
        close_block(&mut stack);
    }
    stack.pop().and_then(|top| top.block).unwrap_or_default()
}

/// True if `head` ends with `url` (any case): an unquoted `url(` follows. Only the end is
/// looked at, so a long statement is read in linear time.
fn ends_with_url(head: &str) -> bool {
    let tail = head
        .len()
        .checked_sub(3)
        .and_then(|start| head.get(start..));
    tail.is_some_and(|tail| tail.eq_ignore_ascii_case("url"))
}

/// `statement` with its head trimmed.
fn trimmed(mut statement: Statement) -> Statement {
    statement.head = statement.head.trim().to_string();
    statement
}

/// Ends the innermost open block of `stack` (more than one entry): it becomes a statement
/// of the block around it.
fn close_block(stack: &mut Vec<Statement>) {
    if let Some(mut closed) = stack.pop()
        && let Some(open) = stack.last_mut()
    {
        closed.block.get_or_insert_default();
        open.block.get_or_insert_default().push(trimmed(closed));
    }
}

/// Skips a string literal opened by `quote` (escapes and interpolations `#{…}` included,
/// ended by the quote or a line break); returns the number of line breaks consumed.
fn skip_string(chars: &mut Peekable<Chars<'_>>, quote: char) -> usize {
    let mut lines = 0;
    while let Some(c) = chars.next() {
        match c {
            '\\' => lines += usize::from(chars.next() == Some('\n')),
            '\n' => return lines + 1,
            '#' if chars.peek() == Some(&'{') => {
                lines += copy_interpolation(chars, &mut String::new());
            }
            c if c == quote => return lines,
            _ => {}
        }
    }
    lines
}

/// Copies an unquoted `url(` argument up to its `)` into `head` (a quoted one is left to
/// the caller); returns the number of line breaks consumed.
fn skip_unquoted_url(chars: &mut Peekable<Chars<'_>>, head: &mut String) -> usize {
    while chars.next_if(|c| c.is_whitespace() && *c != '\n').is_some() {}
    if matches!(chars.peek(), Some('"' | '\'')) {
        return 0;
    }
    let mut lines = 0;
    for c in chars.by_ref() {
        lines += usize::from(c == '\n');
        head.push(c);
        if c == ')' {
            break;
        }
    }
    lines
}

/// Copies an interpolation `{…}` (after its `#`) with nested braces into `head`, its
/// string literals emptied (a brace or quote in one does not count); returns the number
/// of line breaks consumed.
fn copy_interpolation(chars: &mut Peekable<Chars<'_>>, head: &mut String) -> usize {
    let (mut depth, mut lines) = (0usize, 0);
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                lines += skip_string(chars, c);
                head.push_str("\"\"");
                continue;
            }
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        lines += usize::from(c == '\n');
        head.push(c);
        if depth == 0 {
            break;
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kind, cycle, line and always-flag [`unconditional_recursion`] finds in `scss`.
    fn recursion(scss: &str) -> Option<(RecursionKind, String, usize)> {
        unconditional_recursion(scss).map(|found| {
            assert!(found.always);
            (found.kind, found.cycle.join(" > "), found.line)
        })
    }

    const MIXIN: RecursionKind = RecursionKind::Mixin;
    const FUNCTION: RecursionKind = RecursionKind::Function;

    #[test]
    fn unconditional_recursion_is_found() {
        let found = |kind, cycle: &str, line| Some((kind, cycle.to_string(), line));
        // The audit's examples (F02): a mixin without its base case, a function.
        let pad =
            "@mixin pad($n) { padding: $n; @include pad($n - 1); }\nbody { @include pad(3px); }\n";
        assert_eq!(recursion(pad), found(MIXIN, "pad > pad", 1));
        assert_eq!(
            recursion("@function f(){@return f();}\na { b: f(); }"),
            found(FUNCTION, "f > f", 1)
        );
        // Through others, on the line of the @include; `-` and `_` are the same name.
        let mutual = "@mixin a {\n  color: red;\n  @include b;\n}\n@mixin b { @include my_c(); }\n\
                      @mixin my-c { x: y; @include a }\nb { @include b; }\n";
        assert_eq!(recursion(mutual), found(MIXIN, "a > b > my-c > a", 3));
        // Nested rules and @media blocks always run; so does a call before any @return.
        let nested = "@mixin m {\n  #{$sel} a { @media print { @include m; } }\n}\n@include m;";
        assert_eq!(recursion(nested), found(MIXIN, "m > m", 2));
        let before = "@function f($n) {\n  $x: 1 + f($n);\n  @return $x;\n}\n$y: f(1);";
        assert_eq!(recursion(before), found(FUNCTION, "f > f", 2));
        let debug = "@function g() { @debug g(); @return 1; }\na { b: g() }";
        assert_eq!(recursion(debug), found(FUNCTION, "g > g", 1));
        // An unquoted url() with `//` is no comment.
        let url = "@mixin u { background: url(http://x.example/y.png); @include u; }\n@include u;";
        assert_eq!(recursion(url), found(MIXIN, "u > u", 1));
        // The left side of `or` always runs; mixins come first.
        let left = "@function f($n) { @return f($n - 1) or true; }\na { b: f(1) }\n\
                    @mixin m { @include m; }\n@include m;";
        assert_eq!(recursion(left), found(MIXIN, "m > m", 3));
        let left = "@function f($n) { @return (f($n - 1) and 1) or 2; }\na { b: f(1) }";
        assert_eq!(recursion(left), found(FUNCTION, "f > f", 1));
        // Reached through another definition, or a control directive at the top level.
        let via = "@mixin a { @include a; }\n@mixin b($x) { @if $x { @include a; } }\n\
                   @if $c { b { @include b(1); } }";
        assert_eq!(recursion(via), found(MIXIN, "a > a", 1));
    }

    #[test]
    fn guarded_unused_or_foreign_recursion_is_not_unconditional() {
        for scss in [
            // A base case.
            "@mixin pad($n) { @if $n > 0 { padding: $n; @include pad($n - 1); } }\n@include pad(1);",
            "@mixin pad($n) { @if $n == 0 { a: b; } @else { @include pad($n - 1); } }\n@include pad(1);",
            "@mixin l($list) { @each $x in $list { @include l(nth($x, 1)); } }\n@include l(1);",
            "@function f($n) {\n  @if $n <= 0 { @return 0; }\n  @return f($n - 1);\n}\na { b: f(1) }",
            "@function f($n) { @while $n > 0 { @return f($n - 1); } @return 0; }\na { b: f(1) }",
            "@function f($n) { @return if($n > 0, f($n - 1), 0); }\na { b: f(1) }",
            "@function f() { @error \"no\"; @return f(); }\na { b: f() }",
            // The legacy `@elseif` (audit review).
            "@mixin pad($n) {\n  @if $n <= 0 { padding: 0; }\n  @elseif $n > 0 { margin: $n; @include pad($n - 1); }\n}\nbody { @include pad(3); }",
            "@function f($n) { @if $n < 0 { $n: 0; } @elseif $n == 0 { @return 0; } @return f($n - 1); }\na { b: f(1) }",
            // `and`/`or` evaluate their right-hand side only when needed (audit review).
            "@function f($n) { @return $n <= 0 or f($n - 1); }\nh1 { x: f(3); }",
            "@function f($n) { @return $n == 0 and 0 or f($n - 1); }\nh1 { x: f(3); }",
            "@function f($n) { @return ($n > 0 and f($n - 1)) + 1; }\nh1 { x: f(3); }",
            // An interpolation in a string with a brace and a quote (audit review).
            "@mixin m($n) { @if $n > 0 { content: \"#{\"}\"}\"; @include m($n - 1); } }\n@include m(2);",
            "@mixin m($n) { @if $n > 0 { a#{\"{\"} { b: c; } @include m($n - 1); } }\n@include m(2);",
            // A content block may not run.
            "@mixin m { @include other { @include m; } }\n@include m;",
            // Never used (audit review): Sass never runs it.
            "@mixin unused { @include unused; }\na { b: c; }",
            "@function g() { @return g(); }\n@mixin m { @include m; }",
            "@mixin a { @include b; }\n@mixin b { @include a; }",
            // Another module's mixin, a variable, text.
            "@use \"m\";\n@mixin pad { @include m.pad; }\n@include pad;",
            "@function f() { @return $f + m.f(1) + -f(2); }\na { b: f() }",
            "/* @mixin x { @include x; } */ // @mixin y { @include y; }\n\
             @mixin z { content: \"@include z;\"; } @mixin w { content: '}'; }\n\
             @include z; @include w;",
            // Not a definition, or invalid.
            "a { @include a; }",
            "@mixin { @include x; }",
            "@mixin m { @include m",
            "}}} @mixin m { a: b }",
            "",
        ] {
            assert_eq!(recursion(scss), None, "{scss}");
        }
    }

    #[test]
    fn other_files_reach_definitions() {
        let partial = "@function f($n) { @return g($n); }\n@function g($n) { @return f($n); }\n";
        assert_eq!(unconditional_recursion(partial), None);
        let elsewhere = Refs::from([(FUNCTION, "f".to_string())]);
        let (found, refs) = always_recursion(partial, &elsewhere);
        let found = found.unwrap();
        assert_eq!(
            (found.kind, found.cycle.join(" > ")),
            (FUNCTION, "f > g > f".into())
        );
        assert!(refs.contains(&(FUNCTION, "g".to_string())), "{refs:?}");
        // A namespaced reference names another file's member.
        let (_, refs) = always_recursion(
            "@use \"fns\";\na { b: fns.f(1); @include m.x; }",
            &Refs::new(),
        );
        assert!(refs.contains(&(FUNCTION, "f".to_string())), "{refs:?}");
        assert!(refs.contains(&(MIXIN, "x".to_string())), "{refs:?}");
    }

    #[test]
    fn hints_name_the_recursion_that_never_ended() {
        let files = |list: &[(&str, &str)]| -> Vec<(String, String)> {
            list.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        // The audit's conditional examples: a forgotten decrement, a base case never met.
        let forgotten = "@mixin pad($n) { @if $n > 0 { padding: $n; @include pad($n); } }\n\
                         body { @include pad(3px); }\n";
        let Some(Hint::One(at, found)) = recursion_hint(&files(&[("a", forgotten)])) else {
            panic!("one recursion");
        };
        assert_eq!(
            (at, found.kind, found.always, found.line),
            (0, MIXIN, false, 1)
        );
        let spacing = "@function spacing($n) {\n  @if $n == 0 { @return 0px; }\n  \
                       @return spacing($n - 2) + 4px;\n}\n.a { margin: spacing(3); }";
        let Some(Hint::One(_, found)) = recursion_hint(&files(&[("a", spacing)])) else {
            panic!("one recursion");
        };
        assert_eq!((found.kind, found.always, found.line), (FUNCTION, false, 3));
        let message = found.message(spacing);
        assert!(
            message.starts_with(
                "line 3: Sass ran out of stack, most likely in the recursion of @function \
                 spacing, which calls itself here: the condition meant to end it is never met"
            ),
            "{message}"
        );
        assert!(
            message.ends_with("\n 3 |   @return spacing($n - 2) + 4px;"),
            "{message}"
        );
        // An unconditional one comes first, in a later file reached from the first.
        let entry = "@use \"p\";\n@mixin x($n) { @if $n { @include x($n); } }\n@include x(1);\n\
                     a { b: p.f(); }";
        let partial = "@function f() { @return f(); }";
        let Some(Hint::One(at, found)) = recursion_hint(&files(&[("a", entry), ("p", partial)]))
        else {
            panic!("one recursion");
        };
        assert_eq!((at, found.kind, found.always), (1, FUNCTION, true));
        assert_eq!(recursion_hint(&files(&[("a", "a { b: c; }")])), None);
    }

    #[test]
    fn several_conditional_recursions_are_all_candidates() {
        // Wave-8 review of the A2 fixes: a correct recursive helper (Bootstrap's
        // str-replace recurses inside an @if) must not be blamed for another function's
        // overflow, whichever comes first.
        let files = |list: &[(&str, &str)]| -> Vec<(String, String)> {
            list.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        let entry = "@use \"lib\";\n@use \"spacing\";\n\
                     .a { content: lib.str-replace(\"a-b\", \"-\", \"_\"); \
                     margin: spacing.spacing(3); }\n";
        let lib = "@function str-replace($string, $search, $replace: \"\") {\n  \
                   $index: str-index($string, $search);\n  @if $index {\n    \
                   @return str-slice($string, 1, $index - 1) + $replace + \
                   str-replace(str-slice($string, $index + str-length($search)), $search, \
                   $replace);\n  }\n  @return $string;\n}\n";
        let spacing = "@function spacing($n) {\n  @if $n == 0 { @return 0px; }\n  \
                       @return spacing($n - 2) + 4px;\n}\n";
        let hint = recursion_hint(&files(&[("e", entry), ("l", lib), ("s", spacing)]));
        let Some(Hint::Candidates(found)) = hint else {
            panic!("{hint:?}");
        };
        let summaries: Vec<String> = found
            .iter()
            .map(|(at, recursion)| recursion.summary(["e", "l", "s"][*at]))
            .collect();
        assert_eq!(
            summaries,
            ["@function str-replace (l:4)", "@function spacing (s:3)"]
        );
        // Both in one file, the correct one first.
        let both = format!("{lib}{spacing}.a {{ x: str-replace(\"a\", \"b\"); y: spacing(3); }}");
        let Some(Hint::Candidates(found)) = recursion_hint(&files(&[("b", &both)])) else {
            panic!("two candidates");
        };
        assert_eq!(found.len(), 2);
        // A mutual recursion is one candidate, not one per definition on it.
        let mutual = "@mixin a($n) { @if $n { @include b($n); } }\n\
                      @mixin b($n) { @if $n { @include a($n); } }\nx { @include a(1); }\n";
        let Some(Hint::One(_, found)) = recursion_hint(&files(&[("m", mutual)])) else {
            panic!("one recursion");
        };
        assert_eq!(found.summary("m"), "@mixin a (m:1, through b)");
        // Unreached recursions are no candidates.
        let unused = format!("{lib}{spacing}.a {{ y: spacing(3); }}");
        let Some(Hint::One(_, found)) = recursion_hint(&files(&[("u", &unused)])) else {
            panic!("one recursion");
        };
        assert_eq!(found.cycle, ["spacing", "spacing"]);
    }

    #[test]
    fn recursion_messages_quote_the_call() {
        let scss = "@mixin a {\n  @include b;\n}\n@mixin b { @include a; }\n@include a;\n";
        let message = unconditional_recursion(scss).unwrap().message(scss);
        assert_eq!(
            message,
            "line 2: @mixin a always includes itself through b, so Sass recurses until it runs \
             out of stack; make the recursion depend on an @if that ends it\n 2 |   \
             @include b;"
        );
        let scss = "@function f() { @return f(); }\na { b: f() }";
        let message = unconditional_recursion(scss).unwrap().message(scss);
        assert!(
            message.starts_with("line 1: @function f always calls itself before any @return, so"),
            "{message}"
        );
    }

    #[test]
    fn calls_after_and_or_and_in_if_are_conditional() {
        let calls = |expression: &str| -> Vec<(String, bool)> {
            called_names(expression, true)
                .into_iter()
                .map(|call| (call.name, call.always))
                .collect()
        };
        let pairs = |list: &[(&str, bool)]| -> Vec<(String, bool)> {
            list.iter().map(|(n, a)| (n.to_string(), *a)).collect()
        };
        assert_eq!(calls("a(1) or b(2)"), pairs(&[("a", true), ("b", false)]));
        assert_eq!(
            calls("(a() or b()) + c() and d()"),
            pairs(&[("a", true), ("b", false), ("c", true), ("d", false)])
        );
        assert_eq!(
            calls("if(x(), y(), z()) + w()"),
            pairs(&[("x", false), ("y", false), ("z", false), ("w", true)])
        );
        // Not operators: part of a name, a variable, a string's text.
        assert_eq!(
            calls("$or + for-or() + mand() + \"\" + e()"),
            pairs(&[("for-or", true), ("mand", true), ("e", true)])
        );
    }

    #[test]
    fn long_statements_are_read_in_linear_time() {
        // Generated design tokens: one map statement of about 1 MB with many `(`.
        let mut scss = String::from("$tokens: (\n");
        for i in 0..15_000 {
            let _ = writeln!(
                scss,
                "  \"token-{i}\": (light: rgba(1, 2, 3, 0.5), dark: rgba(4, 5, 6, 0.5)),"
            );
        }
        scss.push_str(");\n@mixin m { @include m; }\n@include m;\n");
        assert!(scss.len() > 1_000_000, "{}", scss.len());
        let start = std::time::Instant::now();
        let found = unconditional_recursion(&scss).unwrap();
        assert_eq!(found.cycle, ["m", "m"]);
        // Quadratic reading took minutes in a debug build; linear takes well under a second.
        let took = start.elapsed();
        assert!(took < std::time::Duration::from_secs(20), "{took:?}");
    }

    #[test]
    fn no_embedded_stylesheet_recurses_unconditionally() {
        let mut checked = 0;
        for path in crate::vendor::list("") {
            if !path.to_ascii_lowercase().ends_with(".scss") {
                continue;
            }
            let text = crate::vendor::get_str(&path).unwrap();
            assert_eq!(unconditional_recursion(&text), None, "{path}");
            checked += 1;
        }
        assert!(checked > 10, "only {checked} embedded .scss files");
    }
}
