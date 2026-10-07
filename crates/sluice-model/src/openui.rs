//! OpenUI Lang, the closed component language of a question's `ui` (drawn in the browser by
//! the vendored lang-core) and of a project's board (parsed, checked and drawn here and by the
//! dashboard's server). A program is one statement per line, `name = Component(arg, ...)`; the
//! first statement is the root. Arguments are positional, in each component's signature order.
//! Values are strings (double or single quoted, JSON escapes), numbers, `true`, `false`,
//! `null`, `[arrays]`, `{key: value}` objects, components and names of other statements. Lines
//! starting `//` or `#`, and Markdown fences, are skipped.

use std::collections::{BTreeMap, BTreeSet};

/// The most bytes of a board program.
pub const MAX_PROGRAM_BYTES: usize = 64 * 1024;
/// The most statements of one program.
pub const MAX_STATEMENTS: usize = 400;
/// The most components a program draws once its names are resolved.
pub const MAX_NODES: usize = 4000;
const MAX_DEPTH: usize = 48;

/// One parsed value. A `Component` keeps the line its call started on.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
    Ref(String),
    Component(Component),
}
#[derive(Debug, Clone, PartialEq)]
pub struct Component {
    pub name: String,
    pub args: Vec<Value>,
    pub line: usize,
}
impl Component {
    /// The argument at `index` (in signature order), `None` when absent or `null`.
    pub fn arg(&self, index: usize) -> Option<&Value> {
        self.args.get(index).filter(|v| !matches!(v, Value::Null))
    }
    pub fn str_arg(&self, index: usize) -> Option<&str> {
        match self.arg(index) {
            Some(Value::String(s)) => Some(s),
            _ => None,
        }
    }
    pub fn num_arg(&self, index: usize) -> Option<f64> {
        match self.arg(index) {
            Some(Value::Number(n)) => Some(*n),
            _ => None,
        }
    }
    pub fn bool_arg(&self, index: usize) -> Option<bool> {
        match self.arg(index) {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        }
    }
    /// A `string[]` argument (non-strings left out).
    pub fn strings_arg(&self, index: usize) -> Vec<String> {
        match self.arg(index) {
            Some(Value::Array(items)) => items.iter().filter_map(scalar_text).collect(),
            _ => vec![],
        }
    }
    /// A `Component[]` argument.
    pub fn components_arg(&self, index: usize) -> Vec<&Component> {
        match self.arg(index) {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|v| match v {
                    Value::Component(c) => Some(c),
                    _ => None,
                })
                .collect(),
            _ => vec![],
        }
    }
}
/// A string, number or boolean as text; anything else is `None`.
pub fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(number_text(*n)),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}
pub fn number_text(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}
impl Value {
    /// The value as JSON (a component as `{"component": name}`; resolved programs have no refs).
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Null => serde_json::Value::Null,
            Self::Bool(b) => (*b).into(),
            Self::Number(n) => serde_json::Number::from_f64(*n)
                .map(|n| {
                    if n.as_f64()
                        .is_some_and(|f| f.fract() == 0.0 && f.abs() < 9e15)
                    {
                        serde_json::Value::from(n.as_f64().unwrap_or(0.0) as i64)
                    } else {
                        serde_json::Value::Number(n)
                    }
                })
                .unwrap_or(serde_json::Value::Null),
            Self::String(s) => s.clone().into(),
            Self::Array(items) => items.iter().map(Self::to_json).collect(),
            Self::Object(fields) => fields
                .iter()
                .map(|(k, v)| (k.clone(), v.to_json()))
                .collect::<serde_json::Map<_, _>>()
                .into(),
            Self::Ref(name) => serde_json::json!({ "ref": name }),
            Self::Component(c) => serde_json::json!({ "component": c.name }),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    pub name: String,
    pub value: Value,
    pub line: usize,
}
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Program {
    pub statements: Vec<Statement>,
}

/// A problem on a line (1-based), as `line N: message`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Problem {
    pub line: usize,
    pub message: String,
}
impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

// ---- parsing -------------------------------------------------------------------------------

struct Parser<'a> {
    chars: Vec<char>,
    at: usize,
    line: usize,
    depth: usize,
    _src: &'a str,
}
type Parsed<T> = Result<T, Problem>;
fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '$'
}
fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}
impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            chars: src.chars().collect(),
            at: 0,
            line: 1,
            depth: 0,
            _src: src,
        }
    }
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.at += 1;
        if c == '\n' {
            self.line += 1;
        }
        Some(c)
    }
    fn fail<T>(&self, message: impl Into<String>) -> Parsed<T> {
        Err(Problem {
            line: self.line,
            message: message.into(),
        })
    }
    /// Spaces and tabs only: a statement ends at its line's end.
    fn blanks(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\r')) {
            self.bump();
        }
    }
    /// Inside brackets newlines are whitespace too, and so are `//` comments.
    fn space(&mut self) {
        loop {
            match self.peek() {
                Some(' ' | '\t' | '\r' | '\n') => {
                    self.bump();
                }
                Some('/') if self.chars.get(self.at + 1) == Some(&'/') => {
                    while !matches!(self.peek(), None | Some('\n')) {
                        self.bump();
                    }
                }
                _ => return,
            }
        }
    }
    fn ident(&mut self) -> Option<String> {
        if !self.peek().is_some_and(is_ident_start) {
            return None;
        }
        let mut out = String::new();
        while let Some(c) = self.peek().filter(|c| is_ident(*c)) {
            out.push(c);
            self.bump();
        }
        Some(out)
    }
    fn value(&mut self) -> Parsed<Value> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return self.fail("nested too deeply");
        }
        let value = self.value_inner();
        self.depth -= 1;
        value
    }
    fn value_inner(&mut self) -> Parsed<Value> {
        let line = self.line;
        match self.peek() {
            None => self.fail("a value is missing"),
            Some('"' | '\'') => self.string().map(Value::String),
            Some('[') => {
                self.bump();
                let mut items = vec![];
                loop {
                    self.space();
                    if self.peek() == Some(']') {
                        self.bump();
                        return Ok(Value::Array(items));
                    }
                    items.push(self.value()?);
                    self.space();
                    match self.bump() {
                        Some(',') => {}
                        Some(']') => return Ok(Value::Array(items)),
                        _ => return self.fail("expected , or ] in a list"),
                    }
                }
            }
            Some('{') => {
                self.bump();
                let mut fields: Vec<(String, Value)> = vec![];
                loop {
                    self.space();
                    if self.peek() == Some('}') {
                        self.bump();
                        return Ok(Value::Object(fields));
                    }
                    let key = match self.peek() {
                        Some('"' | '\'') => self.string()?,
                        _ => match self.ident() {
                            Some(key) => key,
                            None => return self.fail("expected a key in an object"),
                        },
                    };
                    self.space();
                    if self.bump() != Some(':') {
                        return self.fail(format!("expected : after the key {key}"));
                    }
                    self.space();
                    let value = self.value()?;
                    if fields.iter().any(|(k, _)| *k == key) {
                        return self.fail(format!("the key {key} is given twice"));
                    }
                    fields.push((key, value));
                    self.space();
                    match self.bump() {
                        Some(',') => {}
                        Some('}') => return Ok(Value::Object(fields)),
                        _ => return self.fail("expected , or } in an object"),
                    }
                }
            }
            Some(c) if c == '-' || c.is_ascii_digit() => self.number(),
            Some(c) if is_ident_start(c) => {
                let name = self.ident().unwrap_or_default();
                let save = (self.at, self.line);
                self.blanks();
                if self.peek() == Some('(') {
                    self.bump();
                    let mut args = vec![];
                    loop {
                        self.space();
                        if self.peek() == Some(')') {
                            self.bump();
                            break;
                        }
                        args.push(self.value()?);
                        self.space();
                        match self.bump() {
                            Some(',') => {}
                            Some(')') => break,
                            _ => return self.fail(format!("expected , or ) in {name}(…)")),
                        }
                    }
                    return Ok(Value::Component(Component { name, args, line }));
                }
                (self.at, self.line) = save;
                Ok(match name.as_str() {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    "null" => Value::Null,
                    _ => Value::Ref(name),
                })
            }
            Some(c) => self.fail(format!("unexpected {c:?}")),
        }
    }
    fn number(&mut self) -> Parsed<Value> {
        let mut text = String::new();
        while let Some(c) = self
            .peek()
            .filter(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'))
        {
            text.push(c);
            self.bump();
        }
        match text.parse::<f64>() {
            Ok(n) if n.is_finite() => Ok(Value::Number(n)),
            _ => self.fail(format!("{text} is not a number")),
        }
    }
    fn string(&mut self) -> Parsed<String> {
        let quote = self.bump().unwrap_or('"');
        let mut out = String::new();
        loop {
            match self.bump() {
                None | Some('\n') => return self.fail("a string is not closed on its line"),
                Some(c) if c == quote => return Ok(out),
                Some('\\') => match self.bump() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('b') => out.push('\u{8}'),
                    Some('f') => out.push('\u{c}'),
                    Some('/') => out.push('/'),
                    Some('u') => {
                        let hex: String = (0..4).filter_map(|_| self.bump()).collect();
                        let code = u32::from_str_radix(&hex, 16).ok();
                        match code.and_then(char::from_u32) {
                            Some(c) => out.push(c),
                            None => return self.fail(format!("bad escape \\u{hex}")),
                        }
                    }
                    Some(c @ ('"' | '\'' | '\\')) => out.push(c),
                    Some(c) => return self.fail(format!("bad escape \\{c}")),
                    None => return self.fail("a string is not closed on its line"),
                },
                Some(c) => out.push(c),
            }
        }
    }
    /// After a statement that failed: on to the next line that starts a statement.
    fn resync(&mut self) {
        loop {
            while !matches!(self.peek(), None | Some('\n')) {
                self.bump();
            }
            if self.bump().is_none() {
                return;
            }
            let save = (self.at, self.line);
            self.blanks();
            let starts = self.ident().is_some() && {
                self.blanks();
                self.peek() == Some('=')
            };
            (self.at, self.line) = save;
            if starts {
                return;
            }
        }
    }
}

/// Parse a program. Every line that is not a statement, comment or fence is a problem; the
/// statements that did parse are kept.
pub fn parse(src: &str) -> (Program, Vec<Problem>) {
    let mut p = Parser::new(src);
    let mut program = Program::default();
    let mut problems = vec![];
    loop {
        // blank lines, comments and fences between statements
        loop {
            p.blanks();
            match p.peek() {
                Some('\n') => {
                    p.bump();
                }
                Some('#') => p.resync_line(),
                Some('/') if p.chars.get(p.at + 1) == Some(&'/') => p.resync_line(),
                Some('`') if p.chars[p.at..].starts_with(&['`', '`', '`']) => p.resync_line(),
                _ => break,
            }
        }
        if p.peek().is_none() {
            break;
        }
        let line = p.line;
        let statement = (|| {
            let Some(name) = p.ident() else {
                return p.fail("not a statement (expected name = Component(…))");
            };
            p.blanks();
            if p.bump() != Some('=') {
                return p.fail(format!("expected = after {name}"));
            }
            p.blanks();
            let value = p.value()?;
            p.blanks();
            if p.peek() == Some(';') {
                p.bump();
                p.blanks();
            }
            if p.peek() == Some('/') && p.chars.get(p.at + 1) == Some(&'/') {
                while !matches!(p.peek(), None | Some('\n')) {
                    p.bump();
                }
            }
            match p.peek() {
                None | Some('\n') => Ok(Statement { name, value, line }),
                Some(c) => p.fail(format!("unexpected {c:?} after the statement")),
            }
        })();
        match statement {
            Ok(statement) => program.statements.push(statement),
            Err(problem) => {
                problems.push(problem);
                p.resync();
            }
        }
    }
    (program, problems)
}
impl Parser<'_> {
    fn resync_line(&mut self) {
        while !matches!(self.peek(), None | Some('\n')) {
            self.bump();
        }
    }
}

// ---- the vocabulary ------------------------------------------------------------------------

/// A prop's type, as the signatures in the docs spell it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropType {
    String,
    Number,
    Boolean,
    Record,
    Components,
    Strings,
    StringRows,
    OneOf(&'static [&'static str]),
    SomeOf(&'static [&'static str]),
}
impl PropType {
    pub fn spelling(self) -> String {
        let quoted = |values: &[&str]| {
            values
                .iter()
                .map(|v| format!("\"{v}\""))
                .collect::<Vec<_>>()
                .join(" | ")
        };
        match self {
            Self::String => "string".into(),
            Self::Number => "number".into(),
            Self::Boolean => "boolean".into(),
            Self::Record => "Record<string, any>".into(),
            Self::Components => "Component[]".into(),
            Self::Strings => "string[]".into(),
            Self::StringRows => "string[][]".into(),
            Self::OneOf(values) => quoted(values),
            Self::SomeOf(values) => format!("({})[]", quoted(values)),
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Prop {
    pub name: &'static str,
    pub ty: PropType,
    pub required: bool,
}
const fn req(name: &'static str, ty: PropType) -> Prop {
    Prop {
        name,
        ty,
        required: true,
    }
}
const fn opt(name: &'static str, ty: PropType) -> Prop {
    Prop {
        name,
        ty,
        required: false,
    }
}
#[derive(Debug, Clone, Copy)]
pub struct ComponentSpec {
    pub name: &'static str,
    pub props: &'static [Prop],
    /// Data-bound: resolved by the dashboard's server when it draws a board.
    pub data: bool,
}
impl ComponentSpec {
    /// The signature as the docs list it: `Name(a: string, b?: number)`.
    pub fn signature(&self) -> String {
        let props = self
            .props
            .iter()
            .map(|p| {
                format!(
                    "{}{}: {}",
                    p.name,
                    if p.required { "" } else { "?" },
                    p.ty.spelling()
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("{}({props})", self.name)
    }
}
use PropType as T;
const RULES: Prop = opt("rules", T::Strings);
/// The unit states the units view reports (`UnitState`).
pub const UNIT_STATES: &[&str] = &[
    "running", "failed", "settled", "blocked", "queued", "pending",
];

/// What a question's `ui` may use (assets/openui.js draws exactly these).
pub const QUESTION_COMPONENTS: &[ComponentSpec] = &[
    ComponentSpec {
        name: "Stack",
        props: &[
            req("children", T::Components),
            opt("direction", T::OneOf(&["col", "row"])),
        ],
        data: false,
    },
    ComponentSpec {
        name: "Heading",
        props: &[req("text", T::String), opt("level", T::Number)],
        data: false,
    },
    ComponentSpec {
        name: "Text",
        props: &[
            req("text", T::String),
            opt("tone", T::OneOf(&["default", "muted"])),
        ],
        data: false,
    },
    ComponentSpec {
        name: "Callout",
        props: &[
            req("text", T::String),
            opt("variant", T::OneOf(&["info", "success", "warning"])),
            opt("title", T::String),
        ],
        data: false,
    },
    ComponentSpec {
        name: "Table",
        props: &[
            req("columns", T::Strings),
            req("rows", T::StringRows),
            opt("caption", T::String),
        ],
        data: false,
    },
    ComponentSpec {
        name: "Separator",
        props: &[],
        data: false,
    },
    ComponentSpec {
        name: "Form",
        props: &[
            req("name", T::String),
            req("fields", T::Components),
            req("buttons", T::Components),
        ],
        data: false,
    },
    ComponentSpec {
        name: "Input",
        props: &[
            req("name", T::String),
            opt("label", T::String),
            opt("placeholder", T::String),
            opt("type", T::OneOf(&["text", "number", "email", "url"])),
            opt("value", T::String),
            RULES,
        ],
        data: false,
    },
    ComponentSpec {
        name: "Textarea",
        props: &[
            req("name", T::String),
            opt("label", T::String),
            opt("placeholder", T::String),
            opt("value", T::String),
            RULES,
            opt("rows", T::Number),
        ],
        data: false,
    },
    ComponentSpec {
        name: "Select",
        props: &[
            req("name", T::String),
            req("options", T::Strings),
            opt("label", T::String),
            opt("value", T::String),
            RULES,
        ],
        data: false,
    },
    ComponentSpec {
        name: "Radio",
        props: &[
            req("name", T::String),
            req("options", T::Strings),
            opt("label", T::String),
            opt("value", T::String),
            RULES,
        ],
        data: false,
    },
    ComponentSpec {
        name: "Checkbox",
        props: &[
            req("name", T::String),
            req("label", T::String),
            opt("checked", T::Boolean),
        ],
        data: false,
    },
    ComponentSpec {
        name: "Button",
        props: &[
            req("label", T::String),
            opt("action", T::String),
            opt("params", T::Record),
            opt("variant", T::OneOf(&["primary", "secondary"])),
        ],
        data: false,
    },
];
/// What a board adds: components the server fills from the project when it draws the page.
pub const DATA_COMPONENTS: &[ComponentSpec] = &[
    ComponentSpec {
        name: "Units",
        props: &[opt("state", T::SomeOf(UNIT_STATES))],
        data: true,
    },
    ComponentSpec {
        name: "StepStatus",
        props: &[req("step", T::String)],
        data: true,
    },
    ComponentSpec {
        name: "Output",
        props: &[req("step", T::String), req("field", T::String)],
        data: true,
    },
    ComponentSpec {
        name: "Metric",
        props: &[req("label", T::String), req("query", T::String)],
        data: true,
    },
    ComponentSpec {
        name: "Query",
        props: &[req("query", T::String), opt("caption", T::String)],
        data: true,
    },
    ComponentSpec {
        name: "Chart",
        props: &[
            req("kind", T::OneOf(&["bar", "line"])),
            req("query", T::String),
            opt("caption", T::String),
        ],
        data: true,
    },
    ComponentSpec {
        name: "Slot",
        props: &[req("key", T::String), opt("fallback", T::String)],
        data: true,
    },
    ComponentSpec {
        name: "Markdown",
        props: &[req("text", T::String)],
        data: false,
    },
    ComponentSpec {
        name: "LatestMessage",
        props: &[req("from", T::String), opt("chars", T::Number)],
        data: true,
    },
];
/// The most slots a project's board keeps (`board_slot_set`).
pub const MAX_BOARD_SLOTS: usize = 64;
/// The most bytes of one slot's markdown.
pub const MAX_SLOT_BYTES: usize = 16 * 1024;
/// The most characters a LatestMessage shows; its default.
pub const MAX_MESSAGE_CHARS: usize = 4000;
pub const DEFAULT_MESSAGE_CHARS: usize = 280;
/// A board slot's key: a lowercase letter or digit, then up to 63 of `a-z 0-9 _ . -`.
pub fn valid_slot_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && key.len() <= 64
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'))
}
pub const SLOT_KEY_RULE: &str =
    "a slot key is a lowercase letter or digit, then up to 63 of a-z, 0-9, _, . and -";
/// The board's whole vocabulary: the question components, then the data components.
pub fn board_components() -> impl Iterator<Item = &'static ComponentSpec> {
    QUESTION_COMPONENTS.iter().chain(DATA_COMPONENTS)
}
pub fn board_spec(name: &str) -> Option<&'static ComponentSpec> {
    board_components().find(|c| c.name == name)
}

// ---- checking ------------------------------------------------------------------------------

/// A board program parsed, checked against the board vocabulary and resolved: names replaced
/// by their statements' values. `root` is the first statement's component.
#[derive(Debug, Clone, PartialEq)]
pub struct Board {
    pub root: Component,
}
/// The data components of a board, in drawing order, and its buttons (each with the form it
/// is in), in drawing order: the dashboard numbers buttons by this order.
impl Board {
    pub fn walk<'a>(&'a self, visit: &mut impl FnMut(&'a Component, Option<&'a str>)) {
        fn go<'a>(
            c: &'a Component,
            form: Option<&'a str>,
            visit: &mut impl FnMut(&'a Component, Option<&'a str>),
        ) {
            visit(c, form);
            let form = if c.name == "Form" {
                c.str_arg(0).or(form)
            } else {
                form
            };
            for arg in &c.args {
                go_value(arg, form, visit);
            }
        }
        fn go_value<'a>(
            v: &'a Value,
            form: Option<&'a str>,
            visit: &mut impl FnMut(&'a Component, Option<&'a str>),
        ) {
            match v {
                Value::Component(c) => go(c, form, visit),
                Value::Array(items) => items.iter().for_each(|i| go_value(i, form, visit)),
                _ => {}
            }
        }
        go(&self.root, None, visit);
    }
    /// Every Button with the Form it is in, in drawing order.
    pub fn buttons(&self) -> Vec<(&Component, Option<&str>)> {
        let mut out = vec![];
        self.walk(&mut |c, form| {
            if c.name == "Button" {
                out.push((c, form));
            }
        });
        out
    }
}

/// Parse and check a board program. Every problem is reported with its line: a line that is
/// not a statement, an unknown component, a missing, extra or mistyped argument, a name used
/// but never defined (or defined twice, or in a cycle), and a statement nothing uses.
pub fn check_board(src: &str) -> Result<Board, Vec<Problem>> {
    if src.len() > MAX_PROGRAM_BYTES {
        return Err(vec![Problem {
            line: 1,
            message: format!("the program is over {} KiB", MAX_PROGRAM_BYTES / 1024),
        }]);
    }
    let (program, mut problems) = parse(src);
    if program.statements.len() > MAX_STATEMENTS {
        problems.push(Problem {
            line: program.statements[MAX_STATEMENTS].line,
            message: format!("a program has at most {MAX_STATEMENTS} statements"),
        });
    }
    let mut names: BTreeMap<&str, &Statement> = BTreeMap::new();
    for statement in &program.statements {
        if let Some(first) = names.get(statement.name.as_str()) {
            problems.push(Problem {
                line: statement.line,
                message: format!(
                    "{} is already defined on line {}",
                    statement.name, first.line
                ),
            });
        } else {
            names.insert(&statement.name, statement);
        }
    }
    let Some(first) = program.statements.first() else {
        if problems.is_empty() {
            problems.push(Problem {
                line: 1,
                message: "the program has no statement".into(),
            });
        }
        problems.sort();
        return Err(problems);
    };
    let mut resolver = Resolver {
        names: &names,
        used: BTreeSet::new(),
        stack: vec![],
        nodes: 0,
        problems: vec![],
    };
    let root = resolver.resolve(&first.value, first.line);
    let mut used = std::mem::take(&mut resolver.used);
    problems.append(&mut resolver.problems);
    used.insert(first.name.clone());
    for statement in &program.statements {
        if !used.contains(&statement.name) {
            problems.push(Problem {
                line: statement.line,
                message: format!("{} is defined but never used", statement.name),
            });
        }
    }
    let root = match root {
        Value::Component(c) => Some(c),
        _ => {
            problems.push(Problem {
                line: first.line,
                message: format!("the root {} is not a component", first.name),
            });
            None
        }
    };
    if let Some(root) = &root {
        check_component(root, &mut problems);
    }
    problems.sort();
    problems.dedup();
    match root {
        Some(root) if problems.is_empty() => Ok(Board { root }),
        _ => Err(problems),
    }
}

struct Resolver<'a> {
    names: &'a BTreeMap<&'a str, &'a Statement>,
    used: BTreeSet<String>,
    stack: Vec<String>,
    nodes: usize,
    problems: Vec<Problem>,
}
impl Resolver<'_> {
    fn resolve(&mut self, value: &Value, line: usize) -> Value {
        match value {
            Value::Ref(name) => {
                let Some(statement) = self.names.get(name.as_str()) else {
                    self.problems.push(Problem {
                        line,
                        message: format!("{name} is used but never defined"),
                    });
                    return Value::Null;
                };
                if self.stack.contains(name) {
                    self.problems.push(Problem {
                        line,
                        message: format!("{name} refers to itself"),
                    });
                    return Value::Null;
                }
                self.used.insert(name.clone());
                self.stack.push(name.clone());
                let out = self.resolve(&statement.value, statement.line);
                self.stack.pop();
                out
            }
            Value::Array(items) => {
                Value::Array(items.iter().map(|v| self.resolve(v, line)).collect())
            }
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(k, v)| (k.clone(), self.resolve(v, line)))
                    .collect(),
            ),
            Value::Component(c) => {
                self.nodes += 1;
                if self.nodes == MAX_NODES + 1 {
                    self.problems.push(Problem {
                        line: c.line,
                        message: format!("the program draws over {MAX_NODES} components"),
                    });
                }
                if self.nodes > MAX_NODES {
                    return Value::Null;
                }
                Value::Component(Component {
                    name: c.name.clone(),
                    args: c.args.iter().map(|v| self.resolve(v, c.line)).collect(),
                    line: c.line,
                })
            }
            other => other.clone(),
        }
    }
}

fn check_component(c: &Component, problems: &mut Vec<Problem>) {
    let mut fail = |message: String| {
        problems.push(Problem {
            line: c.line,
            message,
        })
    };
    let Some(spec) = board_spec(&c.name) else {
        fail(format!("{} is not a board component", c.name));
        return;
    };
    if c.args.len() > spec.props.len() {
        fail(format!(
            "{} takes at most {} arguments: {}",
            c.name,
            spec.props.len(),
            spec.signature()
        ));
    }
    for (index, prop) in spec.props.iter().enumerate() {
        let value = c.args.get(index).unwrap_or(&Value::Null);
        if matches!(value, Value::Null) {
            if prop.required {
                fail(format!(
                    "{} needs {} ({})",
                    c.name,
                    prop.name,
                    spec.signature()
                ));
            }
            continue;
        }
        if let Err(why) = check_type(prop.ty, value) {
            fail(format!(
                "{}: {} must be {} ({why})",
                c.name,
                prop.name,
                prop.ty.spelling()
            ));
        }
    }
    match c.name.as_str() {
        "Slot" => {
            if let Some(Value::String(key)) = c.args.first()
                && !valid_slot_key(key)
            {
                fail(format!(
                    "Slot: \"{key}\" is not a slot key ({SLOT_KEY_RULE})"
                ));
            }
        }
        "LatestMessage" => {
            if let Some(Value::String(from)) = c.args.first()
                && from.trim().is_empty()
            {
                fail(
                    "LatestMessage: from names a sender (a step id or a name such as orchestrator)"
                        .into(),
                );
            }
            if let Some(Value::Number(n)) = c.args.get(1)
                && !(n.fract() == 0.0 && *n >= 1.0 && *n <= MAX_MESSAGE_CHARS as f64)
            {
                fail(format!(
                    "LatestMessage: chars must be a whole number from 1 to {MAX_MESSAGE_CHARS}"
                ));
            }
        }
        _ => {}
    }
    for arg in &c.args {
        visit_components(arg, &mut |child| check_component(child, problems));
    }
}
fn visit_components(value: &Value, visit: &mut impl FnMut(&Component)) {
    match value {
        Value::Component(c) => visit(c),
        Value::Array(items) => items.iter().for_each(|v| visit_components(v, visit)),
        Value::Object(fields) => fields.iter().for_each(|(_, v)| visit_components(v, visit)),
        _ => {}
    }
}
fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
        Value::Ref(_) => "a name",
        Value::Component(_) => "a component",
    }
}
fn check_type(ty: PropType, value: &Value) -> Result<(), String> {
    let got = || format!("got {}", kind(value));
    match (ty, value) {
        (T::String, Value::String(_))
        | (T::Number, Value::Number(_))
        | (T::Boolean, Value::Bool(_))
        | (T::Record, Value::Object(_)) => Ok(()),
        (T::OneOf(values), Value::String(s)) if values.contains(&s.as_str()) => Ok(()),
        (T::OneOf(_), Value::String(s)) => Err(format!("got \"{s}\"")),
        (T::Components, Value::Array(items)) => {
            match items.iter().find(|v| !matches!(v, Value::Component(_))) {
                Some(other) => Err(format!("an item is {}", kind(other))),
                None => Ok(()),
            }
        }
        (T::Strings, Value::Array(items)) => {
            match items.iter().find(|v| !matches!(v, Value::String(_))) {
                Some(other) => Err(format!("an item is {}", kind(other))),
                None => Ok(()),
            }
        }
        (T::SomeOf(values), Value::Array(items)) => {
            for item in items {
                match item {
                    Value::String(s) if values.contains(&s.as_str()) => {}
                    Value::String(s) => return Err(format!("got \"{s}\"")),
                    other => return Err(format!("an item is {}", kind(other))),
                }
            }
            Ok(())
        }
        (T::StringRows, Value::Array(rows)) => {
            for row in rows {
                let Value::Array(cells) = row else {
                    return Err(format!("a row is {}", kind(row)));
                };
                if let Some(cell) = cells.iter().find(|c| scalar_text(c).is_none()) {
                    return Err(format!("a cell is {}", kind(cell)));
                }
            }
            Ok(())
        }
        _ => Err(got()),
    }
}

/// A field rule (`required`, `email`, `url`, `numeric`, `min:N`, `max:N`, `minLength:N`,
/// `maxLength:N`) against a field's value: the message when it fails.
pub fn rule_fails(rule: &str, value: &serde_json::Value) -> Option<String> {
    let text = match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        serde_json::Value::Bool(b) => {
            return (rule == "required" && !b).then(|| "Required.".into());
        }
        other => other.to_string(),
    };
    let (name, arg) = rule.split_once(':').unwrap_or((rule, ""));
    let n: Option<f64> = arg.trim().parse().ok();
    let empty = text.trim().is_empty();
    match name {
        "required" if empty => Some("Required.".into()),
        _ if empty => None,
        "email" if !text.contains('@') || text.contains(char::is_whitespace) => {
            Some("Enter an email address.".into())
        }
        "url" if !(text.starts_with("http://") || text.starts_with("https://")) => {
            Some("Enter a URL.".into())
        }
        "numeric" if text.trim().parse::<f64>().is_err() => Some("Enter a number.".into()),
        "min" | "max" => {
            let (Some(limit), Ok(v)) = (n, text.trim().parse::<f64>()) else {
                return None;
            };
            if name == "min" && v < limit {
                Some(format!("At least {}.", number_text(limit)))
            } else if name == "max" && v > limit {
                Some(format!("At most {}.", number_text(limit)))
            } else {
                None
            }
        }
        "minLength" | "maxLength" => {
            let limit = n? as usize;
            let len = text.chars().count();
            if name == "minLength" && len < limit {
                Some(format!("At least {limit} characters."))
            } else if name == "maxLength" && len > limit {
                Some(format!("At most {limit} characters."))
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_every_bad_line() {
        let src = "root = Stack([a, Nope(1)])\nthis is prose\na = Text(1)\nb = Text(\"never used\")\nc = Metric(\"x\")\n";
        let errors: Vec<String> = check_board(src)
            .unwrap_err()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(
            errors.iter().any(|e| e.starts_with("line 1: Nope is not")),
            "{errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.starts_with("line 2: ")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.starts_with("line 3: Text: text must be string")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("b is defined but never used")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("c is defined but never used")),
            "{errors:?}"
        );
    }

    #[test]
    fn refuses_cycles_and_unknown_names() {
        let errors = check_board("root = Stack([a])\na = Stack([root, z])\n").unwrap_err();
        let text: Vec<String> = errors.iter().map(ToString::to_string).collect();
        assert!(
            text.iter().any(|e| e.contains("refers to itself")),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|e| e.contains("z is used but never defined")),
            "{text:?}"
        );
    }

    #[test]
    fn data_components_check_their_arguments() {
        assert!(check_board("root = Units([\"running\", \"failed\"])").is_ok());
        assert!(check_board("root = Units([\"sleeping\"])").is_err());
        assert!(check_board("root = Chart(\"pie\", \"SELECT 1, 2\")").is_err());
        assert!(check_board("root = Chart(\"bar\", \"SELECT 'a', 2\", \"caption\")").is_ok());
    }

    #[test]
    fn slots_markdown_and_messages_check_their_arguments() {
        let ok = "root = Stack([a, b, c, d, e])\na = Slot(\"phase\")\nb = Slot(\"needs-sam.v2_1\", \"Nothing yet.\")\nc = Markdown(\"**bold** [x](https://x)\")\nd = LatestMessage(\"tests-main\")\ne = LatestMessage(\"orchestrator\", 120)";
        assert!(check_board(ok).is_ok(), "{:?}", check_board(ok).err());
        let lines = |src: &str| -> Vec<String> {
            check_board(src)
                .unwrap_err()
                .iter()
                .map(ToString::to_string)
                .collect()
        };
        assert_eq!(
            lines("root = Slot(\"Phase\")"),
            [format!(
                "line 1: Slot: \"Phase\" is not a slot key ({SLOT_KEY_RULE})"
            )]
        );
        assert!(
            lines("root = Slot()")[0]
                .contains("Slot needs key (Slot(key: string, fallback?: string))")
        );
        assert!(lines("root = Markdown([\"x\"])")[0].contains("Markdown: text must be string"));
        assert!(
            lines("root = LatestMessage(\"a\", 2.5)")[0].contains("chars must be a whole number")
        );
        assert!(lines("root = LatestMessage(\"a\", 5000)")[0].contains("from 1 to 4000"));
        assert!(lines("root = LatestMessage(\" \")")[0].contains("from names a sender"));
        assert!(valid_slot_key(&"a".repeat(64)) && !valid_slot_key(&"a".repeat(65)));
        assert!(!valid_slot_key("") && !valid_slot_key("_a") && !valid_slot_key("a b"));
    }

    #[test]
    fn rules_read_like_the_question_forms() {
        assert!(rule_fails("required", &serde_json::json!("")).is_some());
        assert!(rule_fails("min:3", &serde_json::json!(2)).is_some());
        assert!(rule_fails("maxLength:3", &serde_json::json!("abcd")).is_some());
        assert!(rule_fails("email", &serde_json::json!("a@b")).is_none());
    }
}
