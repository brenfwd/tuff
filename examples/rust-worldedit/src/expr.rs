// SPDX-License-Identifier: MIT
//! Expression language for //generate, //deform and `=` masks: arithmetic,
//! comparisons, logic, assignment, a function library, and if/while/for with
//! a step budget and a nesting limit so no input can run away.

use std::collections::HashMap;
use std::rc::Rc;

pub const MAX_STEPS: u64 = 2_000;

thread_local! {
    static SPENT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

pub fn spent() -> u64 {
    SPENT.with(std::cell::Cell::get)
}

pub fn reset_spent() {
    SPENT.with(|s| s.set(0));
}
const MAX_DEPTH: usize = 48;
const MAX_LEN: usize = 4096;
const PRELUDE: [&str; 3] = ["x", "y", "z"];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Bin {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    Near,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Un {
    Neg,
    Not,
}

#[derive(Clone, Debug)]
enum Node {
    Num(f64),
    Var(usize),
    Un(Un, Box<Node>),
    Bin(Bin, Box<Node>, Box<Node>),
    And(Box<Node>, Box<Node>),
    Or(Box<Node>, Box<Node>),
    Cond(Box<Node>, Box<Node>, Box<Node>),
    Assign(usize, Option<Bin>, Box<Node>),
    Call(&'static Func, Vec<Node>),
    If(Box<Node>, Box<Node>, Option<Box<Node>>),
    While(Box<Node>, Box<Node>),
    For(Box<Node>, Box<Node>, Box<Node>, Box<Node>),
    Block(Vec<Node>),
    Break,
    Continue,
    Return(Box<Node>),
}

#[derive(Debug)]
struct Func {
    name: &'static str,
    arity: (usize, usize),
    run: fn(&mut Run, &[f64]) -> f64,
}

#[derive(Clone, Debug)]
pub struct Expr {
    root: Node,
    slots: usize,
}

enum Flow {
    Break,
    Continue,
    Return(f64),
    Fail(String),
}

type Val = Result<f64, Flow>;

pub struct Run {
    vars: Vec<f64>,
    steps: u64,
    rng: u64,
}

impl Run {
    fn next(&mut self) -> f64 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        (mix(self.rng) >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn hash(seed: i64, x: i64, y: i64, z: i64) -> u64 {
    let mut h = mix(seed as u64 ^ 0x51_7C_C1_B7_27_22_0A_95);
    for c in [x, y, z] {
        h = mix(h ^ (c as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    }
    h
}

fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

fn grad(seed: i64, ix: i64, iy: i64, iz: i64, x: f64, y: f64, z: f64) -> f64 {
    let (u, v) = match hash(seed, ix, iy, iz) % 12 {
        0 => (x, y),
        1 => (-x, y),
        2 => (x, -y),
        3 => (-x, -y),
        4 => (x, z),
        5 => (-x, z),
        6 => (x, -z),
        7 => (-x, -z),
        8 => (y, z),
        9 => (-y, z),
        10 => (y, -z),
        _ => (-y, -z),
    };
    u + v
}

fn gradient_noise(seed: i64, x: f64, y: f64, z: f64) -> f64 {
    let (fx, fy, fz) = (x.floor(), y.floor(), z.floor());
    let (ix, iy, iz) = (fx as i64, fy as i64, fz as i64);
    let (dx, dy, dz) = (x - fx, y - fy, z - fz);
    let (u, v, w) = (fade(dx), fade(dy), fade(dz));
    let corner = |a: i64, b: i64, c: i64| {
        grad(
            seed,
            ix + a,
            iy + b,
            iz + c,
            dx - a as f64,
            dy - b as f64,
            dz - c as f64,
        )
    };
    let lerp = |t: f64, a: f64, b: f64| a + t * (b - a);
    let x1 = lerp(u, corner(0, 0, 0), corner(1, 0, 0));
    let x2 = lerp(u, corner(0, 1, 0), corner(1, 1, 0));
    let x3 = lerp(u, corner(0, 0, 1), corner(1, 0, 1));
    let x4 = lerp(u, corner(0, 1, 1), corner(1, 1, 1));
    (lerp(w, lerp(v, x1, x2), lerp(v, x3, x4)) / 1.5).clamp(-1.0, 1.0)
}

fn fractal(a: &[f64], ridged: bool) -> f64 {
    let seed = a[0] as i64;
    let freq = a[4];
    let octaves = (a[5].round() as i64).clamp(1, 16);
    let persistence = a.get(6).copied().unwrap_or(0.5);
    let (mut sum, mut amp, mut f, mut norm) = (0.0, 1.0, freq, 0.0);
    for o in 0..octaves {
        let n = gradient_noise(seed + o, a[1] * f, a[2] * f, a[3] * f);
        sum += amp * if ridged { 1.0 - 2.0 * n.abs() } else { n };
        norm += amp;
        amp *= persistence;
        f *= 2.0;
    }
    sum / norm
}

fn voronoi(a: &[f64]) -> f64 {
    let seed = a[0] as i64;
    let freq = a[4];
    let (x, y, z) = (a[1] * freq, a[2] * freq, a[3] * freq);
    let (cx, cy, cz) = (x.floor() as i64, y.floor() as i64, z.floor() as i64);
    let mut best = f64::MAX;
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                let (gx, gy, gz) = (cx + dx, cy + dy, cz + dz);
                let h = hash(seed, gx, gy, gz);
                let unit = |s: u32| ((h >> s) & 0xFFFF) as f64 / 65536.0;
                let (px, py, pz) = (
                    gx as f64 + unit(0),
                    gy as f64 + unit(16),
                    gz as f64 + unit(32),
                );
                let d = (px - x).powi(2) + (py - y).powi(2) + (pz - z).powi(2);
                best = best.min(d);
            }
        }
    }
    best.sqrt().min(1.0) * 2.0 - 1.0
}

macro_rules! f1 {
    ($name:literal, $f:expr) => {
        Func {
            name: $name,
            arity: (1, 1),
            run: |_, a| ($f)(a[0]),
        }
    };
}

static FUNCS: &[Func] = &[
    f1!("sin", f64::sin),
    f1!("cos", f64::cos),
    f1!("tan", f64::tan),
    f1!("asin", f64::asin),
    f1!("acos", f64::acos),
    f1!("atan", f64::atan),
    f1!("sinh", f64::sinh),
    f1!("cosh", f64::cosh),
    f1!("tanh", f64::tanh),
    f1!("sqrt", f64::sqrt),
    f1!("cbrt", f64::cbrt),
    f1!("abs", f64::abs),
    f1!("exp", f64::exp),
    f1!("ln", f64::ln),
    f1!("log", f64::ln),
    f1!("log10", f64::log10),
    f1!("floor", f64::floor),
    f1!("ceil", f64::ceil),
    f1!("round", f64::round),
    f1!("rint", f64::round_ties_even),
    f1!("sign", |v: f64| if v == 0.0 { 0.0 } else { v.signum() }),
    f1!("toRadians", f64::to_radians),
    f1!("toDegrees", f64::to_degrees),
    Func {
        name: "atan2",
        arity: (2, 2),
        run: |_, a| a[0].atan2(a[1]),
    },
    Func {
        name: "pow",
        arity: (2, 2),
        run: |_, a| a[0].powf(a[1]),
    },
    Func {
        name: "hypot",
        arity: (2, 3),
        run: |_, a| a.iter().map(|v| v * v).sum::<f64>().sqrt(),
    },
    Func {
        name: "min",
        arity: (2, 2),
        run: |_, a| a[0].min(a[1]),
    },
    Func {
        name: "max",
        arity: (2, 2),
        run: |_, a| a[0].max(a[1]),
    },
    Func {
        name: "clamp",
        arity: (3, 3),
        run: |_, a| a[0].max(a[1]).min(a[2].max(a[1])),
    },
    Func {
        name: "rand",
        arity: (0, 0),
        run: |r, _| r.next(),
    },
    Func {
        name: "random",
        arity: (0, 0),
        run: |r, _| r.next(),
    },
    Func {
        name: "randint",
        arity: (1, 1),
        run: |r, a| (r.next() * a[0].max(1.0)).floor(),
    },
    Func {
        name: "perlin",
        arity: (6, 7),
        run: |_, a| fractal(a, false),
    },
    Func {
        name: "ridgedmulti",
        arity: (6, 7),
        run: |_, a| fractal(a, true),
    },
    Func {
        name: "voronoi",
        arity: (5, 5),
        run: |_, a| voronoi(a),
    },
];

fn func(name: &str) -> Option<&'static Func> {
    FUNCS.iter().find(|f| f.name == name)
}

#[derive(Clone, PartialEq, Debug)]
enum Tok {
    Num(f64),
    Id(String),
    Op(&'static str),
}

const OPS: &[&str] = &[
    "+=", "-=", "*=", "/=", "%=", "^=", "==", "!=", "~=", "<=", ">=", "&&", "||", "+", "-", "*",
    "/", "%", "^", "<", ">", "=", "!", "?", ":", "(", ")", "{", "}", ",", ";",
];

fn lex(text: &str) -> Result<Vec<Tok>, String> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || (c == '.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            let start = i;
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
                let mut j = i + 1;
                if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
                    j += 1;
                }
                if j < b.len() && b[j].is_ascii_digit() {
                    while j < b.len() && b[j].is_ascii_digit() {
                        j += 1;
                    }
                    i = j;
                }
            }
            let n = text[start..i]
                .parse()
                .map_err(|_| format!("Bad number {}", &text[start..i]))?;
            out.push(Tok::Num(n));
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(Tok::Id(text[start..i].to_owned()));
        } else {
            let op = OPS
                .iter()
                .find(|op| text[i..].starts_with(**op))
                .ok_or_else(|| format!("Unexpected character '{c}'"))?;
            out.push(Tok::Op(op));
            i += op.len();
        }
    }
    Ok(out)
}

struct Parser {
    toks: Vec<Tok>,
    at: usize,
    depth: usize,
    names: HashMap<String, usize>,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }

    fn is_op(&self, op: &str) -> bool {
        matches!(self.peek(), Some(Tok::Op(o)) if *o == op)
    }

    fn eat(&mut self, op: &str) -> bool {
        let hit = self.is_op(op);
        if hit {
            self.at += 1;
        }
        hit
    }

    fn expect(&mut self, op: &str) -> Result<(), String> {
        match self.eat(op) {
            true => Ok(()),
            false => Err(format!("Expected '{op}'")),
        }
    }

    fn is_word(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Tok::Id(w)) if w == word)
    }

    fn enter(&mut self) -> Result<(), String> {
        self.depth += 1;
        match self.depth > MAX_DEPTH {
            true => Err("Expression is nested too deeply".to_owned()),
            false => Ok(()),
        }
    }

    fn slot(&mut self, name: &str) -> usize {
        let next = self.names.len();
        *self.names.entry(name.to_owned()).or_insert(next)
    }

    fn program(&mut self) -> Result<Node, String> {
        let mut all = Vec::new();
        while self.peek().is_some() {
            if self.eat(";") {
                continue;
            }
            all.push(self.statement()?);
            if self.peek().is_some() && !self.eat(";") && !self.prev_closed_block() {
                return Err("Expected ';'".to_owned());
            }
        }
        Ok(Node::Block(all))
    }

    fn prev_closed_block(&self) -> bool {
        self.at > 0 && self.toks[self.at - 1] == Tok::Op("}")
    }

    fn statement(&mut self) -> Result<Node, String> {
        self.enter()?;
        let node = self.statement_inner();
        self.depth -= 1;
        node
    }

    fn statement_inner(&mut self) -> Result<Node, String> {
        if self.eat("{") {
            let mut all = Vec::new();
            while !self.is_op("}") {
                if self.peek().is_none() {
                    return Err("Missing '}'".to_owned());
                }
                if self.eat(";") {
                    continue;
                }
                all.push(self.statement()?);
                if !self.is_op("}") && !self.eat(";") && !self.prev_closed_block() {
                    return Err("Expected ';'".to_owned());
                }
            }
            self.expect("}")?;
            return Ok(Node::Block(all));
        }
        if self.is_word("if") {
            self.at += 1;
            self.expect("(")?;
            let cond = self.expr()?;
            self.expect(")")?;
            let then = self.statement()?;
            let save = self.at;
            self.eat(";");
            let other = match self.is_word("else") {
                true => {
                    self.at += 1;
                    Some(Box::new(self.statement()?))
                }
                false => {
                    self.at = save;
                    None
                }
            };
            return Ok(Node::If(Box::new(cond), Box::new(then), other));
        }
        if self.is_word("while") {
            self.at += 1;
            self.expect("(")?;
            let cond = self.expr()?;
            self.expect(")")?;
            let body = self.statement()?;
            return Ok(Node::While(Box::new(cond), Box::new(body)));
        }
        if self.is_word("for") {
            self.at += 1;
            self.expect("(")?;
            let init = self.expr()?;
            self.expect(";")?;
            let cond = self.expr()?;
            self.expect(";")?;
            let inc = self.expr()?;
            self.expect(")")?;
            let body = self.statement()?;
            return Ok(Node::For(
                Box::new(init),
                Box::new(cond),
                Box::new(inc),
                Box::new(body),
            ));
        }
        for (word, node) in [("break", Node::Break), ("continue", Node::Continue)] {
            if self.is_word(word) {
                self.at += 1;
                return Ok(node);
            }
        }
        if self.is_word("return") {
            self.at += 1;
            let value = match self.peek() {
                None | Some(Tok::Op(";" | "}")) => Node::Num(0.0),
                _ => self.expr()?,
            };
            return Ok(Node::Return(Box::new(value)));
        }
        self.expr()
    }

    fn expr(&mut self) -> Result<Node, String> {
        self.enter()?;
        let node = self.assignment();
        self.depth -= 1;
        node
    }

    fn assignment(&mut self) -> Result<Node, String> {
        if let (Some(Tok::Id(name)), Some(Tok::Op(op))) =
            (self.toks.get(self.at), self.toks.get(self.at + 1))
        {
            let how = match *op {
                "=" => Some(None),
                "+=" => Some(Some(Bin::Add)),
                "-=" => Some(Some(Bin::Sub)),
                "*=" => Some(Some(Bin::Mul)),
                "/=" => Some(Some(Bin::Div)),
                "%=" => Some(Some(Bin::Rem)),
                "^=" => Some(Some(Bin::Pow)),
                _ => None,
            };
            if let Some(how) = how {
                let name = name.clone();
                if func(&name).is_some() || is_keyword(&name) {
                    return Err(format!("Cannot assign to {name}"));
                }
                if how.is_some() && !self.names.contains_key(&name) {
                    return Err(format!("Unknown variable {name}"));
                }
                self.at += 2;
                let value = self.assignment()?;
                let slot = self.slot(&name);
                return Ok(Node::Assign(slot, how, Box::new(value)));
            }
        }
        self.ternary()
    }

    fn ternary(&mut self) -> Result<Node, String> {
        let cond = self.or()?;
        if self.eat("?") {
            let a = self.assignment()?;
            self.expect(":")?;
            let b = self.assignment()?;
            return Ok(Node::Cond(Box::new(cond), Box::new(a), Box::new(b)));
        }
        Ok(cond)
    }

    fn or(&mut self) -> Result<Node, String> {
        let mut left = self.and()?;
        while self.eat("||") {
            left = Node::Or(Box::new(left), Box::new(self.and()?));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Node, String> {
        let mut left = self.equality()?;
        while self.eat("&&") {
            left = Node::And(Box::new(left), Box::new(self.equality()?));
        }
        Ok(left)
    }

    fn level(
        &mut self,
        ops: &[(&str, Bin)],
        next: fn(&mut Parser) -> Result<Node, String>,
    ) -> Result<Node, String> {
        let mut left = next(self)?;
        'more: loop {
            for (text, bin) in ops {
                if self.eat(text) {
                    left = Node::Bin(*bin, Box::new(left), Box::new(next(self)?));
                    continue 'more;
                }
            }
            return Ok(left);
        }
    }

    fn equality(&mut self) -> Result<Node, String> {
        self.level(
            &[("==", Bin::Eq), ("!=", Bin::Ne), ("~=", Bin::Near)],
            Parser::comparison,
        )
    }

    fn comparison(&mut self) -> Result<Node, String> {
        self.level(
            &[
                ("<=", Bin::Le),
                (">=", Bin::Ge),
                ("<", Bin::Lt),
                (">", Bin::Gt),
            ],
            Parser::additive,
        )
    }

    fn additive(&mut self) -> Result<Node, String> {
        self.level(&[("+", Bin::Add), ("-", Bin::Sub)], Parser::term)
    }

    fn term(&mut self) -> Result<Node, String> {
        self.level(
            &[("*", Bin::Mul), ("/", Bin::Div), ("%", Bin::Rem)],
            Parser::unary,
        )
    }

    fn unary(&mut self) -> Result<Node, String> {
        self.enter()?;
        let node = if self.eat("-") {
            self.unary().map(|n| Node::Un(Un::Neg, Box::new(n)))
        } else if self.eat("+") {
            self.unary()
        } else if self.eat("!") {
            self.unary().map(|n| Node::Un(Un::Not, Box::new(n)))
        } else {
            self.power()
        };
        self.depth -= 1;
        node
    }

    fn power(&mut self) -> Result<Node, String> {
        let base = self.atom()?;
        if self.eat("^") {
            let exp = self.unary()?;
            return Ok(Node::Bin(Bin::Pow, Box::new(base), Box::new(exp)));
        }
        Ok(base)
    }

    fn atom(&mut self) -> Result<Node, String> {
        let Some(tok) = self.peek().cloned() else {
            return Err("Unexpected end of expression".to_owned());
        };
        self.at += 1;
        match tok {
            Tok::Num(n) => Ok(Node::Num(n)),
            Tok::Op("(") => {
                let inner = self.expr()?;
                self.expect(")")?;
                Ok(inner)
            }
            Tok::Op(op) => Err(format!("Unexpected '{op}'")),
            Tok::Id(name) => self.name(name),
        }
    }

    fn name(&mut self, name: String) -> Result<Node, String> {
        if self.is_op("(") {
            let f = func(&name).ok_or_else(|| format!("Unknown function {name}"))?;
            self.at += 1;
            let mut args = Vec::new();
            if !self.is_op(")") {
                loop {
                    args.push(self.expr()?);
                    if !self.eat(",") {
                        break;
                    }
                }
            }
            self.expect(")")?;
            return match (f.arity.0..=f.arity.1).contains(&args.len()) {
                true => Ok(Node::Call(f, args)),
                false => Err(format!(
                    "{name} takes {} argument(s), got {}",
                    match f.arity.0 == f.arity.1 {
                        true => f.arity.0.to_string(),
                        false => format!("{} to {}", f.arity.0, f.arity.1),
                    },
                    args.len()
                )),
            };
        }
        let constant = match name.as_str() {
            "pi" => Some(std::f64::consts::PI),
            "e" => Some(std::f64::consts::E),
            "true" => Some(1.0),
            "false" => Some(0.0),
            _ => None,
        };
        if let Some(c) = constant {
            return Ok(Node::Num(c));
        }
        if is_keyword(&name) {
            return Err(format!("Unexpected keyword {name}"));
        }
        match self.names.get(&name) {
            Some(slot) => Ok(Node::Var(*slot)),
            None => Err(format!("Unknown variable {name}")),
        }
    }
}

fn is_keyword(word: &str) -> bool {
    matches!(
        word,
        "if" | "else" | "while" | "for" | "break" | "continue" | "return"
    )
}

impl Expr {
    pub fn parse(text: &str) -> Result<Expr, String> {
        if text.len() > MAX_LEN {
            return Err("Expression is too long".to_owned());
        }
        let toks = lex(text)?;
        if toks.is_empty() {
            return Err("Empty expression".to_owned());
        }
        let mut parser = Parser {
            toks,
            at: 0,
            depth: 0,
            names: PRELUDE
                .iter()
                .enumerate()
                .map(|(i, n)| (n.to_string(), i))
                .collect(),
        };
        let root = parser.program()?;
        Ok(Expr {
            root,
            slots: parser.names.len(),
        })
    }

    pub fn run(&self, at: [f64; 3], seed: u64) -> Result<([f64; 3], f64), String> {
        let mut vars = vec![0.0; self.slots];
        vars[..3].copy_from_slice(&at);
        let mut run = Run {
            vars,
            steps: 0,
            rng: seed,
        };
        let outcome = eval(&self.root, &mut run);
        SPENT.with(|s| s.set(s.get() + run.steps));
        let value = match outcome {
            Ok(v) | Err(Flow::Return(v)) => v,
            Err(Flow::Break | Flow::Continue) => 0.0,
            Err(Flow::Fail(why)) => return Err(why),
        };
        Ok(([run.vars[0], run.vars[1], run.vars[2]], value))
    }

    pub fn value(&self, at: [f64; 3], seed: u64) -> Result<f64, String> {
        self.run(at, seed).map(|(_, v)| v)
    }
}

fn truthy(v: f64) -> bool {
    v > 0.0
}

fn flag(b: bool) -> f64 {
    f64::from(u8::from(b))
}

fn eval(node: &Node, run: &mut Run) -> Val {
    run.steps += 1;
    if run.steps > MAX_STEPS {
        return Err(Flow::Fail(
            "Expression ran too long (step limit reached)".to_owned(),
        ));
    }
    Ok(match node {
        Node::Num(n) => *n,
        Node::Var(i) => run.vars[*i],
        Node::Un(Un::Neg, a) => -eval(a, run)?,
        Node::Un(Un::Not, a) => flag(!truthy(eval(a, run)?)),
        Node::Bin(op, a, b) => {
            let (l, r) = (eval(a, run)?, eval(b, run)?);
            binary(*op, l, r)
        }
        Node::And(a, b) => flag(truthy(eval(a, run)?) && truthy(eval(b, run)?)),
        Node::Or(a, b) => flag(truthy(eval(a, run)?) || truthy(eval(b, run)?)),
        Node::Cond(c, a, b) => match truthy(eval(c, run)?) {
            true => eval(a, run)?,
            false => eval(b, run)?,
        },
        Node::Assign(slot, how, value) => {
            let v = eval(value, run)?;
            let v = match how {
                Some(op) => binary(*op, run.vars[*slot], v),
                None => v,
            };
            run.vars[*slot] = v;
            v
        }
        Node::Call(f, args) => {
            let mut vals = Vec::with_capacity(args.len());
            for a in args {
                vals.push(eval(a, run)?);
            }
            (f.run)(run, &vals)
        }
        Node::If(c, a, b) => match (truthy(eval(c, run)?), b) {
            (true, _) => eval(a, run)?,
            (false, Some(b)) => eval(b, run)?,
            (false, None) => 0.0,
        },
        Node::While(c, body) => {
            let mut last = 0.0;
            while truthy(eval(c, run)?) {
                match eval(body, run) {
                    Ok(v) => last = v,
                    Err(Flow::Break) => break,
                    Err(Flow::Continue) => {}
                    Err(other) => return Err(other),
                }
            }
            last
        }
        Node::For(init, c, inc, body) => {
            eval(init, run)?;
            let mut last = 0.0;
            while truthy(eval(c, run)?) {
                match eval(body, run) {
                    Ok(v) => last = v,
                    Err(Flow::Break) => break,
                    Err(Flow::Continue) => {}
                    Err(other) => return Err(other),
                }
                eval(inc, run)?;
            }
            last
        }
        Node::Block(all) => {
            let mut last = 0.0;
            for n in all {
                last = eval(n, run)?;
            }
            last
        }
        Node::Break => return Err(Flow::Break),
        Node::Continue => return Err(Flow::Continue),
        Node::Return(v) => return Err(Flow::Return(eval(v, run)?)),
    })
}

fn binary(op: Bin, l: f64, r: f64) -> f64 {
    match op {
        Bin::Add => l + r,
        Bin::Sub => l - r,
        Bin::Mul => l * r,
        Bin::Div => l / r,
        Bin::Rem => l % r,
        Bin::Pow => l.powf(r),
        Bin::Lt => flag(l < r),
        Bin::Gt => flag(l > r),
        Bin::Le => flag(l <= r),
        Bin::Ge => flag(l >= r),
        Bin::Eq => flag(l == r),
        Bin::Ne => flag(l != r),
        Bin::Near => flag((l - r).abs() < 1e-6 * l.abs().max(r.abs()).max(1.0)),
    }
}

pub fn site_seed(seed: u64, pos: [i32; 3]) -> u64 {
    mix(hash(
        seed as i64,
        pos[0] as i64,
        pos[1] as i64,
        pos[2] as i64,
    ))
}

#[derive(Clone, Debug)]
pub struct Frame {
    base: [f64; 3],
    scale: [f64; 3],
}

impl Frame {
    pub fn raw(base: [f64; 3]) -> Frame {
        Frame {
            base,
            scale: [1.0; 3],
        }
    }

    pub fn unit(min: [i32; 3], max: [i32; 3]) -> Frame {
        let mut frame = Frame::raw([0.0; 3]);
        for a in 0..3 {
            frame.base[a] = (min[a] as f64 + max[a] as f64) / 2.0;
            frame.scale[a] = 2.0 / f64::from((max[a] - min[a]).max(1));
        }
        frame
    }

    pub fn centre(min: [i32; 3], max: [i32; 3]) -> Frame {
        Frame::raw([
            (min[0] as f64 + max[0] as f64) / 2.0,
            (min[1] as f64 + max[1] as f64) / 2.0,
            (min[2] as f64 + max[2] as f64) / 2.0,
        ])
    }

    fn to(&self, pos: [i32; 3]) -> [f64; 3] {
        std::array::from_fn(|a| (pos[a] as f64 - self.base[a]) * self.scale[a])
    }

    fn from(&self, v: [f64; 3]) -> [i32; 3] {
        std::array::from_fn(|a| (v[a] / self.scale[a] + self.base[a]).round() as i32)
    }
}

#[derive(Clone, Debug)]
pub struct Gen {
    pub expr: Rc<Expr>,
    pub frame: Frame,
    pub min: [i32; 3],
    pub max: [i32; 3],
    pub hollow: bool,
}

impl Gen {
    fn solid(&self, pos: [i32; 3], seed: u64) -> bool {
        self.expr
            .value(self.frame.to(pos), site_seed(seed, pos))
            .is_ok_and(truthy)
    }

    pub fn wants(&self, pos: [i32; 3], seed: u64) -> bool {
        if !self.solid(pos, seed) {
            return false;
        }
        !self.hollow
            || (0..6).any(|i| {
                let mut n = pos;
                n[i / 2] += if i % 2 == 0 { -1 } else { 1 };
                let inside = (0..3).all(|a| n[a] >= self.min[a] && n[a] <= self.max[a]);
                !inside || !self.solid(n, seed)
            })
    }

    pub fn check(&self, seed: u64) -> Result<(), String> {
        let mid = self.frame.from([0.0; 3]);
        self.expr.value(self.frame.to(mid), seed).map(|_| ())
    }
}

#[derive(Clone)]
pub struct Deform {
    pub expr: Rc<Expr>,
    pub frame: Frame,
    pub clip: crate::edit::Clip,
}

impl Deform {
    pub fn source(&self, pos: [i32; 3], seed: u64) -> Option<String> {
        let (at, _) = self
            .expr
            .run(self.frame.to(pos), site_seed(seed, pos))
            .ok()?;
        let src = self.frame.from(at);
        Some(match self.clip.at(src) {
            Some(state) => state.to_owned(),
            None => "minecraft:air".to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn val(text: &str) -> f64 {
        Expr::parse(text).unwrap().value([0.0; 3], 1).unwrap()
    }

    fn at(text: &str, p: [f64; 3]) -> f64 {
        Expr::parse(text).unwrap().value(p, 1).unwrap()
    }

    #[test]
    fn arithmetic_precedence_and_power() {
        assert_eq!(val("1 + 2 * 3"), 7.0);
        assert_eq!(val("(1 + 2) * 3"), 9.0);
        assert_eq!(val("2 ^ 3 ^ 2"), 512.0);
        assert_eq!(val("-2 ^ 2"), -4.0);
        assert_eq!(val("7 % 4"), 3.0);
        assert_eq!(val("10 / 4"), 2.5);
        assert!((val("pi") - std::f64::consts::PI).abs() < 1e-12);
    }

    #[test]
    fn comparisons_logic_and_ternary() {
        assert_eq!(val("3 > 2 && 1 < 2"), 1.0);
        assert_eq!(val("3 < 2 || 0"), 0.0);
        assert_eq!(val("!0"), 1.0);
        assert_eq!(val("1 == 1 ? 5 : 6"), 5.0);
        assert_eq!(val("1 != 1 ? 5 : 6"), 6.0);
        assert_eq!(val("0.1 + 0.2 ~= 0.3"), 1.0);
    }

    #[test]
    fn variables_and_assignment() {
        assert_eq!(at("x*x + y*y + z*z", [1.0, 2.0, 3.0]), 14.0);
        assert_eq!(val("a = 2; a += 3; a *= 2; a"), 10.0);
        let (end, v) = Expr::parse("x = x + 1; y = 7; 0")
            .unwrap()
            .run([1.0, 0.0, 0.0], 1)
            .unwrap();
        assert_eq!((end, v), ([2.0, 7.0, 0.0], 0.0));
    }

    #[test]
    fn functions() {
        assert_eq!(val("max(2, min(9, 4))"), 4.0);
        assert_eq!(val("floor(2.7) + ceil(2.1) + round(2.5)"), 8.0);
        assert_eq!(val("abs(-3) + sqrt(16) + pow(2, 3)"), 15.0);
        assert!(val("sin(0) + cos(0)") == 1.0);
        assert_eq!(val("hypot(3, 4)"), 5.0);
    }

    #[test]
    fn control_flow() {
        assert_eq!(val("s = 0; for (i = 0; i < 5; i += 1) s += i; s"), 10.0);
        assert_eq!(
            val("i = 0; while (i < 10) { i += 1; if (i == 4) break; } i"),
            4.0
        );
        assert_eq!(
            val("s = 0; for (i = 0; i < 6; i += 1) { if (i % 2) continue; s += i; } s"),
            6.0
        );
        assert_eq!(val("if (0) 1; else 2"), 2.0);
        assert_eq!(val("if (1) { 3 } else { 4 }"), 3.0);
        assert_eq!(val("return 9; 1"), 9.0);
    }

    #[test]
    fn rand_and_noise_are_deterministic_and_bounded() {
        let e = Expr::parse("rand()").unwrap();
        let a = e.value([0.0; 3], 5).unwrap();
        assert_eq!(a, e.value([0.0; 3], 5).unwrap());
        assert!((0.0..1.0).contains(&a));
        assert_ne!(a, e.value([0.0; 3], 6).unwrap());
        let n = Expr::parse("perlin(1, x, y, z, 0.3, 3, 0.5)").unwrap();
        for i in 0..50 {
            let p = [i as f64 * 0.7, i as f64 * 0.3, 1.1];
            let v = n.value(p, 0).unwrap();
            assert!((-1.0..=1.0).contains(&v), "{v}");
            assert_eq!(v, n.value(p, 0).unwrap());
        }
        let v = Expr::parse("voronoi(1, x, y, z, 0.5)").unwrap();
        assert!((-1.0..=1.0).contains(&v.value([3.3, 1.0, 2.0], 0).unwrap()));
        assert!(Expr::parse("ridgedmulti(1, x, y, z, 0.5, 2)").is_ok());
    }

    #[test]
    fn malformed_input_is_rejected() {
        for bad in [
            "",
            "1 +",
            "(1",
            "1)",
            "foo",
            "sin()",
            "sin(1,2)",
            "nope(1)",
            "1 $ 2",
            "a +=1",
            "if 1",
            "{ 1",
            "x = ",
            "1 2",
            "sin = 3",
            "for (;;) 1",
            "else",
            "1 ? 2",
        ] {
            assert!(Expr::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn budget_and_depth_limits_stop_runaway_input() {
        let forever = Expr::parse("while (1) 1").unwrap();
        let why = forever.value([0.0; 3], 0).unwrap_err();
        assert!(why.contains("too long"), "{why}");
        let nested = "(".repeat(500) + "1" + &")".repeat(500);
        assert!(Expr::parse(&nested).unwrap_err().contains("nested"));
        let unary = "-".repeat(500) + "1";
        assert!(Expr::parse(&unary).is_err());
        let blocks = "{".repeat(500) + &"}".repeat(500);
        assert!(Expr::parse(&blocks).is_err());
        assert!(Expr::parse(&"1+".repeat(3000)).is_err());
    }
}

#[cfg(test)]
mod gen_tests {
    use super::*;

    fn sphere(hollow: bool) -> Gen {
        let (min, max) = ([0, 100, 0], [10, 110, 10]);
        Gen {
            expr: Rc::new(Expr::parse("x*x+y*y+z*z<1").unwrap()),
            frame: Frame::unit(min, max),
            min,
            max,
            hollow,
        }
    }

    #[test]
    fn unit_frame_spans_minus_one_to_one_and_hollow_keeps_only_the_shell() {
        let solid = sphere(false);
        assert!(solid.wants([5, 105, 5], 0) && solid.wants([9, 105, 5], 0));
        assert!(!solid.wants([10, 105, 5], 0) && !solid.wants([0, 100, 0], 0));
        let hollow = sphere(true);
        assert!(!hollow.wants([5, 105, 5], 0) && !hollow.wants([8, 105, 5], 0));
        assert!(hollow.wants([9, 105, 5], 0));
    }
}
