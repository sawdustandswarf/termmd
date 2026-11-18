//! Compiled keymap: what a Wayland client needs to turn (keycode, modifier
//! state) into keysyms. The compositor tracks modifier state for us
//! (`wl_keyboard.modifiers`), so XKB actions are parsed but not executed.

use std::borrow::Cow;
use std::collections::HashMap;

use super::keysym::Keysym;
use super::parse::{self, Expr, Stmt};

/// A modifier mask in the keymap's index space: bits 0–7 are the real
/// modifiers, higher bits are virtual modifiers in declaration order. This is
/// the space `wl_keyboard.modifiers` masks use.
pub type ModMask = u32;

const LOCK: ModMask = 1 << 1;

pub const REAL_MODS: [&str; 8] = ["Shift", "Lock", "Control", "Mod1", "Mod2", "Mod3", "Mod4", "Mod5"];

#[derive(Debug, Clone)]
struct KeyType {
    name: String,
    /// Resolved mask of modifiers this type looks at.
    mask: ModMask,
    entries: Vec<TypeEntry>,
    num_levels: usize,
}

#[derive(Debug, Clone)]
struct TypeEntry {
    /// Resolved modifiers for this entry.
    mods: ModMask,
    /// Entry is active: declared with no modifiers, or its virtual modifiers
    /// resolved to something.
    active: bool,
    level: usize,
    preserve: ModMask,
}

#[derive(Debug, Clone, Default)]
struct Group {
    ty: usize,
    /// Keysyms per level; usually one each.
    levels: Vec<Vec<Keysym>>,
}

#[derive(Debug, Clone, Default)]
struct Key {
    name: String,
    groups: Vec<Group>,
    /// Real modifiers from `modifier_map`.
    modmap: ModMask,
    /// Virtual modifiers bound by interprets (or set explicitly).
    vmodmap: ModMask,
    repeats: bool,
    /// `repeat=` was given in the symbols section.
    explicit_repeat: bool,
}

/// The result of looking up a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyLookup<'a> {
    pub keysyms: Cow<'a, [Keysym]>,
    pub level: usize,
    pub group: usize,
    /// Modifiers used up selecting the level (don't report them as "held"
    /// for shortcuts; e.g. Shift for `A`).
    pub consumed: ModMask,
}

#[derive(Debug, Clone)]
pub struct Keymap {
    /// Modifier names by index (real, then virtual).
    mod_names: Vec<String>,
    /// Resolved mapping for each modifier index (real mods map to themselves).
    mod_map: Vec<ModMask>,
    types: Vec<KeyType>,
    /// Indexed by XKB keycode (evdev + 8).
    keys: Vec<Option<Key>>,
    pub min_keycode: u32,
    pub max_keycode: u32,
}

#[derive(Debug, Clone)]
struct Interpret {
    sym: Option<Keysym>, // None = Any
    pred: Pred,
    mods: ModMask,
    vmod: Option<usize>,
    repeat: bool,
    level_one_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Pred {
    // Order is the matching priority xkbcomp uses.
    Exactly,
    AllOf,
    NoneOf,
    AnyOf,
    AnyOfOrNone,
}

#[derive(Debug)]
pub struct CompileError(pub String);

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CompileError {}

impl From<parse::Error> for CompileError {
    fn from(e: parse::Error) -> Self {
        CompileError(e.to_string())
    }
}

impl Keymap {
    pub fn parse(src: &str) -> Result<Keymap, CompileError> {
        let ast = parse::parse(src)?;
        Compiler::default().compile(&ast)
    }

    pub fn mod_index(&self, name: &str) -> Option<usize> {
        self.mod_names.iter().position(|n| n.eq_ignore_ascii_case(name))
    }

    /// The resolved mask for a named (real or virtual) modifier; 0 if unknown
    /// or unbound.
    pub fn mod_mask(&self, name: &str) -> ModMask {
        self.mod_index(name).map_or(0, |i| self.mod_map[i])
    }

    pub fn mod_names(&self) -> &[String] {
        &self.mod_names
    }

    pub fn key_name(&self, keycode: u32) -> Option<&str> {
        self.key(keycode).map(|k| k.name.as_str())
    }

    fn key(&self, keycode: u32) -> Option<&Key> {
        self.keys.get(keycode as usize).and_then(Option::as_ref)
    }

    /// Whether holding this key should auto-repeat.
    pub fn repeats(&self, keycode: u32) -> bool {
        self.key(keycode).is_some_and(|k| k.repeats)
    }

    /// Look up a key given the effective modifiers and group from the compositor.
    pub fn lookup(&self, keycode: u32, mods: ModMask, group: u32) -> Option<KeyLookup<'_>> {
        let key = self.key(keycode)?;
        if key.groups.is_empty() {
            return None;
        }
        // Default group behaviour: wrap.
        let g = group as usize % key.groups.len();
        let grp = &key.groups[g];
        let ty = &self.types[grp.ty];
        let masked = mods & ty.mask;
        let (level, preserve) =
            ty.entries.iter().find(|e| e.active && e.mods == masked).map_or((0, 0), |e| (e.level, e.preserve));
        let keysyms = grp.levels.get(level).map_or(&[][..], Vec::as_slice);
        let consumed = ty.mask & !preserve;
        // Caps Lock that the type didn't consume uppercases the result.
        let caps = mods & LOCK != 0 && consumed & LOCK == 0;
        let keysyms = if caps && keysyms.iter().any(|k| k.to_upper() != *k) {
            Cow::Owned(keysyms.iter().map(|k| k.to_upper()).collect())
        } else {
            Cow::Borrowed(keysyms)
        };
        Some(KeyLookup { keysyms, level, group: g, consumed })
    }

    /// Debug helper: the type name of a key's group.
    pub fn key_type_name(&self, keycode: u32, group: usize) -> Option<&str> {
        let key = self.key(keycode)?;
        key.groups.get(group).map(|g| self.types[g.ty].name.as_str())
    }
}

// ---------------------------------------------------------------------------

#[derive(Default)]
struct Compiler {
    mod_names: Vec<String>,
    /// Explicit vmod mappings (`Hyper=0x4000`).
    explicit_vmods: HashMap<usize, ModMask>,
    keycodes: HashMap<String, u32>,
    aliases: HashMap<String, String>,
    min_keycode: u32,
    max_keycode: u32,
    raw_types: Vec<(String, Vec<Stmt>)>,
    interprets: Vec<Interpret>,
    keys: Vec<(String, Vec<Expr>)>,
    modmaps: Vec<(String, Vec<Expr>)>,
    interp_defaults: (bool, bool), // (repeat, level_one_only)
}

fn err(msg: impl Into<String>) -> CompileError {
    CompileError(msg.into())
}

impl Compiler {
    fn compile(mut self, ast: &[Stmt]) -> Result<Keymap, CompileError> {
        self.mod_names = REAL_MODS.iter().map(|s| (*s).to_owned()).collect();
        self.min_keycode = 8;
        self.max_keycode = 255;

        // Pass 1: collect declarations from every section.
        for stmt in ast {
            let Stmt::Section(kind, body) = stmt else { continue };
            match kind.as_str() {
                "xkb_keycodes" => self.keycodes_section(body)?,
                "xkb_types" => self.types_section(body)?,
                "xkb_compatibility" | "xkb_compat" | "xkb_compatibility_map" => self.compat_section(body)?,
                "xkb_symbols" => self.symbols_section(body)?,
                _ => {} // xkb_geometry etc.
            }
        }

        // Pass 2: build keys (needs types for automatic type selection).
        let mut types = Vec::new();
        let raw_types = std::mem::take(&mut self.raw_types);
        for (name, body) in &raw_types {
            types.push(self.key_type(name, body)?);
        }
        let type_index = |name: &str| types.iter().position(|t: &KeyType| t.name == name);

        let max = self.max_keycode.max(self.keycodes.values().copied().max().unwrap_or(0));
        let mut keys: Vec<Option<Key>> = vec![None; max as usize + 1];
        for (name, &code) in &self.keycodes {
            keys[code as usize] = Some(Key { name: name.clone(), repeats: true, ..Key::default() });
        }

        let key_defs = std::mem::take(&mut self.keys);
        for (name, items) in &key_defs {
            let Some(code) = self.resolve_key(name) else { continue };
            let key = keys[code as usize].get_or_insert_with(Key::default);
            self.apply_key_items(key, items, &types, &type_index)?;
        }

        let modmaps = std::mem::take(&mut self.modmaps);
        for (m, entries) in &modmaps {
            let Some(bit) = REAL_MODS.iter().position(|r| r.eq_ignore_ascii_case(m)) else { continue };
            for e in entries {
                let code = match e {
                    Expr::KeyName(k) => self.resolve_key(k),
                    other => keysym_of(other).and_then(|ks| find_key_for_sym(&keys, ks)),
                };
                if let Some(k) = code.and_then(|c| keys[c as usize].as_mut()) {
                    k.modmap |= 1 << bit;
                }
            }
        }

        // Interprets: priority order, then bind vmods and repeat to keys.
        let mut interps = std::mem::take(&mut self.interprets);
        interps.sort_by_key(|i| (i.sym.is_none(), i.pred));
        for key in keys.iter_mut().flatten() {
            apply_interprets(key, &interps);
        }

        // Resolve virtual modifiers: OR of modmaps of keys bound to each one.
        let mut mod_map: Vec<ModMask> = (0..self.mod_names.len()).map(|i| if i < 8 { 1 << i } else { 0 }).collect();
        for (v, mapping) in mod_map.iter_mut().enumerate().skip(8) {
            *mapping = match self.explicit_vmods.get(&v) {
                Some(&m) => m,
                None => keys.iter().flatten().filter(|k| k.vmodmap & (1 << v) != 0).fold(0, |acc, k| acc | k.modmap),
            };
        }
        let resolve = |declared: ModMask| -> ModMask {
            (0..32).filter(|i| declared & (1 << i) != 0).fold(0, |acc, i| acc | mod_map.get(i).copied().unwrap_or(0))
        };
        for t in &mut types {
            t.mask = resolve(t.mask);
            for e in &mut t.entries {
                let declared = e.mods;
                e.mods = resolve(declared);
                e.active = declared == 0 || e.mods != 0;
                e.preserve = resolve(e.preserve);
            }
        }

        Ok(Keymap { mod_names: self.mod_names, mod_map, types, keys, min_keycode: self.min_keycode, max_keycode: max })
    }

    fn resolve_key(&self, name: &str) -> Option<u32> {
        let mut n = name;
        for _ in 0..8 {
            if let Some(&c) = self.keycodes.get(n) {
                return Some(c);
            }
            n = self.aliases.get(n)?;
        }
        None
    }

    fn mod_index_or_declare(&mut self, name: &str) -> usize {
        if let Some(i) = self.mod_names.iter().position(|n| n.eq_ignore_ascii_case(name)) {
            return i;
        }
        self.mod_names.push(name.to_owned());
        self.mod_names.len() - 1
    }

    fn declare_vmods(&mut self, mods: &[(String, Option<Expr>)]) -> Result<(), CompileError> {
        for (name, value) in mods {
            let i = self.mod_index_or_declare(name);
            if let Some(v) = value {
                let mask = self.mod_mask(v)?;
                self.explicit_vmods.insert(i, mask);
            }
        }
        Ok(())
    }

    /// Evaluate a modifier expression (`Shift+Lock`, `none`, `all`, `0x4000`)
    /// into a *declared* mask (vmods not yet resolved).
    fn mod_mask(&self, e: &Expr) -> Result<ModMask, CompileError> {
        Ok(match e {
            Expr::Int(v) => *v as ModMask,
            Expr::Ident(n) => match n.to_ascii_lowercase().as_str() {
                "none" => 0,
                "all" | "any" => 0xff,
                _ => match self.mod_names.iter().position(|m| m.eq_ignore_ascii_case(n)) {
                    Some(i) => 1 << i,
                    None => return Err(err(format!("unknown modifier `{n}`"))),
                },
            },
            Expr::Sum(terms) => {
                let mut m = 0;
                for t in terms {
                    m |= self.mod_mask(t)?;
                }
                m
            }
            Expr::Unary('+', inner) => self.mod_mask(inner)?,
            other => return Err(err(format!("bad modifier expression {other:?}"))),
        })
    }

    fn keycodes_section(&mut self, body: &[Stmt]) -> Result<(), CompileError> {
        for s in body {
            match s {
                Stmt::Assign(Expr::KeyName(name), Expr::Int(code)) => {
                    self.keycodes.insert(name.clone(), *code as u32);
                }
                Stmt::Assign(Expr::Ident(f), Expr::Int(v)) if f == "minimum" => self.min_keycode = *v as u32,
                Stmt::Assign(Expr::Ident(f), Expr::Int(v)) if f == "maximum" => self.max_keycode = *v as u32,
                Stmt::Alias(a, b) => {
                    self.aliases.insert(a.clone(), b.clone());
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn types_section(&mut self, body: &[Stmt]) -> Result<(), CompileError> {
        for s in body {
            match s {
                Stmt::VirtualMods(m) => self.declare_vmods(m)?,
                Stmt::Type(name, body) => self.raw_types.push((name.clone(), body.clone())),
                _ => {}
            }
        }
        Ok(())
    }

    fn key_type(&self, name: &str, body: &[Stmt]) -> Result<KeyType, CompileError> {
        let mut mask = 0;
        let mut entries: Vec<TypeEntry> = Vec::new();
        let mut preserves: Vec<(ModMask, ModMask)> = Vec::new();
        let mut num_levels = 1;
        for s in body {
            let Stmt::Assign(lhs, value) = s else { continue };
            match lhs {
                Expr::Ident(f) if f.eq_ignore_ascii_case("modifiers") => mask = self.mod_mask(value)?,
                Expr::Index(f, idx) => match f.as_ident().map(str::to_ascii_lowercase).as_deref() {
                    Some("map") => {
                        let mods = self.mod_mask(idx)?;
                        let level = level_index(value).ok_or_else(|| err(format!("{name}: bad level")))?;
                        num_levels = num_levels.max(level + 1);
                        entries.retain(|e| e.mods != mods);
                        entries.push(TypeEntry { mods, active: true, level, preserve: 0 });
                    }
                    Some("preserve") => preserves.push((self.mod_mask(idx)?, self.mod_mask(value)?)),
                    Some("level_name" | "levelname") => {
                        if let Some(l) = level_index(idx) {
                            num_levels = num_levels.max(l + 1);
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        for (mods, p) in preserves {
            match entries.iter_mut().find(|e| e.mods == mods) {
                Some(e) => e.preserve = p,
                // A preserve without a map entry implies level 1.
                None => entries.push(TypeEntry { mods, active: true, level: 0, preserve: p }),
            }
        }
        Ok(KeyType { name: name.to_owned(), mask, entries, num_levels })
    }

    fn compat_section(&mut self, body: &[Stmt]) -> Result<(), CompileError> {
        self.interp_defaults = (false, false);
        for s in body {
            match s {
                Stmt::VirtualMods(m) => self.declare_vmods(m)?,
                Stmt::Assign(Expr::Field(base, field), value) if base.as_ident() == Some("interpret") => {
                    match field.to_ascii_lowercase().as_str() {
                        "repeat" => self.interp_defaults.0 = bool_value(value).unwrap_or(false),
                        "usemodmapmods" | "usemodmap" => self.interp_defaults.1 = is_level_one(value),
                        _ => {}
                    }
                }
                Stmt::Interpret(matcher, body) => {
                    let interp = self.interpret(matcher, body)?;
                    self.interprets.push(interp);
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn interpret(&mut self, matcher: &Expr, body: &[Stmt]) -> Result<Interpret, CompileError> {
        let (sym_e, pred_e) = match matcher {
            Expr::Sum(t) if t.len() == 2 => (&t[0], Some(&t[1])),
            other => (other, None),
        };
        let sym = match sym_e {
            Expr::Ident(n) if n.eq_ignore_ascii_case("any") => None,
            other => Some(keysym_of(other).ok_or_else(|| err(format!("interpret: bad keysym {other:?}")))?),
        };
        let (pred, mods) = match pred_e {
            None => (Pred::AnyOfOrNone, 0xff),
            Some(Expr::Call(p, args)) => {
                let pred = match p.to_ascii_lowercase().as_str() {
                    "exactly" => Pred::Exactly,
                    "allof" => Pred::AllOf,
                    "noneof" => Pred::NoneOf,
                    "anyof" => Pred::AnyOf,
                    "anyofornone" => Pred::AnyOfOrNone,
                    other => return Err(err(format!("interpret: unknown predicate `{other}`"))),
                };
                let mods = match args.first() {
                    Some(a) => self.mod_mask(a)? & 0xff,
                    None => 0xff,
                };
                (pred, mods)
            }
            Some(e) => (Pred::Exactly, self.mod_mask(e)? & 0xff),
        };
        let mut interp = Interpret {
            sym,
            pred,
            mods,
            vmod: None,
            repeat: self.interp_defaults.0,
            level_one_only: self.interp_defaults.1,
        };
        for s in body {
            let Stmt::Assign(Expr::Ident(field), value) = s else { continue };
            match field.to_ascii_lowercase().as_str() {
                "virtualmodifier" | "virtualmod" => {
                    if let Some(n) = value.as_ident() {
                        interp.vmod = Some(self.mod_index_or_declare(n));
                    }
                }
                "repeat" => interp.repeat = bool_value(value).unwrap_or(interp.repeat),
                "usemodmapmods" | "usemodmap" => interp.level_one_only = is_level_one(value),
                _ => {} // action, locking: the compositor runs actions, not us.
            }
        }
        Ok(interp)
    }

    fn symbols_section(&mut self, body: &[Stmt]) -> Result<(), CompileError> {
        for s in body {
            match s {
                Stmt::Key(name, items) => self.keys.push((name.clone(), items.clone())),
                Stmt::ModMap(m, entries) => self.modmaps.push((m.clone(), entries.clone())),
                Stmt::VirtualMods(m) => self.declare_vmods(m)?,
                _ => {}
            }
        }
        Ok(())
    }

    fn apply_key_items(
        &self,
        key: &mut Key,
        items: &[Expr],
        types: &[KeyType],
        type_index: &dyn Fn(&str) -> Option<usize>,
    ) -> Result<(), CompileError> {
        let mut explicit_types: HashMap<usize, String> = HashMap::new();
        let mut default_type: Option<String> = None;
        let mut next_group = 0;
        let mut groups: Vec<Vec<Vec<Keysym>>> = Vec::new();
        let mut set_group = |g: usize, list: &[Expr]| -> Result<(), CompileError> {
            if groups.len() <= g {
                groups.resize(g + 1, Vec::new());
            }
            groups[g] = list.iter().map(level_keysyms).collect::<Result<_, _>>()?;
            Ok(())
        };

        for item in items {
            match item {
                Expr::List(list) => {
                    set_group(next_group, list)?;
                    next_group += 1;
                }
                Expr::Assign(lhs, value) => match &**lhs {
                    Expr::Ident(f) => match f.to_ascii_lowercase().as_str() {
                        "type" => default_type = string_value(value),
                        "symbols" => {
                            if let Expr::List(l) = &**value {
                                set_group(next_group, l)?;
                                next_group += 1;
                            }
                        }
                        "repeat" | "repeats" | "repeating" => {
                            if let Some(b) = bool_value(value) {
                                key.repeats = b;
                                key.explicit_repeat = true;
                            }
                        }
                        "virtualmods" | "virtualmodifiers" | "vmods" => {
                            key.vmodmap = self.mod_mask(value)?;
                        }
                        _ => {}
                    },
                    Expr::Index(f, idx) => {
                        let g = match &**idx {
                            Expr::Int(n) if *n >= 1 => *n as usize - 1,
                            Expr::Ident(n) => group_index(n).unwrap_or(0),
                            _ => 0,
                        };
                        match f.as_ident().map(str::to_ascii_lowercase).as_deref() {
                            Some("symbols") => {
                                if let Expr::List(l) = &**value {
                                    set_group(g, l)?;
                                }
                            }
                            Some("type") => {
                                if let Some(t) = string_value(value) {
                                    explicit_types.insert(g, t);
                                }
                            }
                            _ => {} // actions[n]: run by the compositor.
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        key.groups = groups
            .into_iter()
            .enumerate()
            .map(|(g, levels)| {
                let name = explicit_types
                    .get(&g)
                    .or(default_type.as_ref())
                    .cloned()
                    .unwrap_or_else(|| automatic_type(&levels).to_owned());
                // Unknown type names fall back to a type with enough levels.
                let ty =
                    type_index(&name).or_else(|| types.iter().position(|t| t.num_levels >= levels.len())).unwrap_or(0);
                Group { ty, levels }
            })
            .collect();
        Ok(())
    }
}

/// xkbcomp's `FindAutomaticType`.
fn automatic_type(levels: &[Vec<Keysym>]) -> &'static str {
    let sym = |i: usize| match levels.get(i).map(Vec::as_slice) {
        Some([s]) => *s,
        _ => Keysym::NO_SYMBOL,
    };
    match levels.len() {
        0 | 1 => "ONE_LEVEL",
        2 => {
            if sym(0).is_lower() && sym(1).is_upper_or_title() {
                "ALPHABETIC"
            } else if sym(0).is_keypad() || sym(1).is_keypad() {
                "KEYPAD"
            } else {
                "TWO_LEVEL"
            }
        }
        3 | 4 => {
            if sym(0).is_lower() && sym(1).is_upper_or_title() {
                if sym(2).is_lower() && sym(3).is_upper_or_title() {
                    "FOUR_LEVEL_ALPHABETIC"
                } else {
                    "FOUR_LEVEL_SEMIALPHABETIC"
                }
            } else if sym(0).is_keypad() || sym(1).is_keypad() {
                "FOUR_LEVEL_KEYPAD"
            } else {
                "FOUR_LEVEL"
            }
        }
        _ => "FOUR_LEVEL",
    }
}

fn apply_interprets(key: &mut Key, interps: &[Interpret]) {
    let mut vmodmap = 0;
    let mut repeat_from_interp = None;
    for (g, group) in key.groups.iter().enumerate() {
        for (l, syms) in group.levels.iter().enumerate() {
            let [sym] = syms.as_slice() else { continue };
            let Some(interp) = find_interp(interps, *sym, l, key.modmap) else { continue };
            let base = g == 0 && l == 0;
            if base {
                repeat_from_interp = Some(interp.repeat);
            }
            if (base || !interp.level_one_only)
                && let Some(v) = interp.vmod
            {
                vmodmap |= 1 << v;
            }
        }
    }
    if key.vmodmap == 0 {
        key.vmodmap = vmodmap;
    }
    if key.explicit_repeat {
        return;
    }
    if let Some(r) = repeat_from_interp {
        key.repeats = r;
    }
    // Keys carrying a modifier keysym on any level don't repeat (this is what
    // libxkbcommon reports, e.g. for <HYPR> = [NoSymbol, Hyper_L]).
    if key.groups.iter().flat_map(|g| &g.levels).flatten().any(|s| s.is_modifier()) {
        key.repeats = false;
    }
}

fn find_interp(interps: &[Interpret], sym: Keysym, level: usize, modmap: ModMask) -> Option<&Interpret> {
    interps.iter().find(|i| {
        if i.sym.is_some_and(|s| s != sym) {
            return false;
        }
        let mods = if i.level_one_only && level != 0 { 0 } else { modmap };
        match i.pred {
            Pred::NoneOf => i.mods & mods == 0,
            Pred::AnyOfOrNone => mods == 0 || i.mods & mods != 0,
            Pred::AnyOf => i.mods & mods != 0,
            Pred::AllOf => i.mods & mods == i.mods,
            Pred::Exactly => i.mods == mods,
        }
    })
}

fn find_key_for_sym(keys: &[Option<Key>], sym: Keysym) -> Option<u32> {
    keys.iter().enumerate().find_map(|(code, k)| {
        let k = k.as_ref()?;
        let g = k.groups.first()?;
        g.levels.iter().any(|l| l.contains(&sym)).then_some(code as u32)
    })
}

/// A keysym as written in a symbols list. Bare digits are keysym names.
fn keysym_of(e: &Expr) -> Option<Keysym> {
    match e {
        Expr::Ident(n) => match n.as_str() {
            "NoSymbol" => Some(Keysym::NO_SYMBOL),
            "VoidSymbol" => Some(Keysym(0xff_ffff)),
            _ => Keysym::from_name(n),
        },
        Expr::Int(v @ 0..=9) => Some(Keysym(b'0' as u32 + *v as u32)),
        Expr::Int(v) => Some(Keysym(*v as u32)),
        _ => None,
    }
}

fn level_keysyms(e: &Expr) -> Result<Vec<Keysym>, CompileError> {
    let one = |e: &Expr| keysym_of(e).ok_or_else(|| err(format!("unknown keysym {e:?}")));
    match e {
        Expr::Set(items) => items.iter().map(one).filter(|r| !matches!(r, Ok(Keysym::NO_SYMBOL))).collect(),
        other => match one(other)? {
            Keysym::NO_SYMBOL => Ok(Vec::new()),
            ks => Ok(vec![ks]),
        },
    }
}

fn level_index(e: &Expr) -> Option<usize> {
    match e {
        Expr::Int(n) if *n >= 1 => Some(*n as usize - 1),
        Expr::Ident(n) => n.to_ascii_lowercase().strip_prefix("level")?.parse::<usize>().ok()?.checked_sub(1),
        _ => None,
    }
}

fn group_index(n: &str) -> Option<usize> {
    n.to_ascii_lowercase().strip_prefix("group")?.parse::<usize>().ok()?.checked_sub(1)
}

fn bool_value(e: &Expr) -> Option<bool> {
    match e {
        Expr::Ident(n) => match n.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" => Some(true),
            "false" | "no" | "off" => Some(false),
            _ => None,
        },
        Expr::Unary('!' | '~', inner) => bool_value(inner).map(|b| !b),
        _ => None,
    }
}

fn is_level_one(e: &Expr) -> bool {
    e.as_ident().is_some_and(|n| n.eq_ignore_ascii_case("level1") || n.eq_ignore_ascii_case("levelone"))
}

fn string_value(e: &Expr) -> Option<String> {
    match e {
        Expr::Str(s) => Some(s.clone()),
        _ => None,
    }
}
