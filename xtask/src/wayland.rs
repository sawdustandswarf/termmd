//! Wayland scanner: protocol XML -> typed Rust for `termmd-linux`'s wire runtime.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::Path;

use crate::xml::{self, Element};

/// Protocol files, in generation order. Paths are relative to `protocols/`.
pub const PROTOCOLS: &[&str] = &[
    "wayland.xml",
    "xdg-shell.xml",
    "xdg-decoration-unstable-v1.xml",
    "viewporter.xml",
    "fractional-scale-v1.xml",
    "cursor-shape-v1.xml",
    "linux-dmabuf-v1.xml",
    "linux-drm-syncobj-v1.xml",
    "primary-selection-unstable-v1.xml",
    "text-input-unstable-v3.xml",
];

/// Rust path of the generated `protocols` module inside `termmd-linux`.
const ROOT: &str = "crate::wayland::protocols";

// ---------------------------------------------------------------------------
// Model

#[derive(Debug)]
pub struct Protocol {
    pub name: String,
    pub file: String,
    pub interfaces: Vec<Interface>,
}

#[derive(Debug)]
pub struct Interface {
    pub name: String,
    pub version: u32,
    pub doc: Doc,
    pub requests: Vec<Message>,
    pub events: Vec<Message>,
    pub enums: Vec<Enum>,
}

#[derive(Debug)]
pub struct Message {
    pub name: String,
    pub destructor: bool,
    pub since: u32,
    pub doc: Doc,
    pub args: Vec<Arg>,
}

#[derive(Debug)]
pub struct Arg {
    pub name: String,
    pub ty: ArgType,
    pub interface: Option<String>,
    pub allow_null: bool,
    pub enum_ref: Option<String>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgType {
    Int,
    Uint,
    Fixed,
    String,
    Object,
    NewId,
    Array,
    Fd,
}

#[derive(Debug)]
pub struct Enum {
    pub name: String,
    pub bitfield: bool,
    pub doc: Doc,
    pub entries: Vec<Entry>,
}

#[derive(Debug)]
pub struct Entry {
    pub name: String,
    pub value: u32,
    pub summary: Option<String>,
}

#[derive(Debug, Default)]
pub struct Doc {
    pub summary: Option<String>,
    pub body: Option<String>,
}

// ---------------------------------------------------------------------------
// XML -> model

pub fn load(path: &Path) -> Result<Protocol, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let root = xml::parse(&src).map_err(|e| format!("{}: {e}", path.display()))?;
    if root.name != "protocol" {
        return Err(format!("{}: root element is not <protocol>", path.display()));
    }
    let file = path.file_name().unwrap().to_string_lossy().into_owned();
    let name = req_attr(&root, "name")?.replace('-', "_");
    let interfaces = root.elements_named("interface").map(interface).collect::<Result<_, _>>()?;
    Ok(Protocol { name, file, interfaces })
}

fn req_attr<'a>(e: &'a Element, name: &str) -> Result<&'a str, String> {
    e.attr(name).ok_or_else(|| format!("<{}> missing attribute `{name}`", e.name))
}

fn doc(e: &Element) -> Doc {
    match e.first("description") {
        None => Doc { summary: e.attr("summary").map(clean_line), body: None },
        Some(d) => {
            let body = d.text();
            let body = body.trim();
            Doc {
                summary: d.attr("summary").or(e.attr("summary")).map(clean_line),
                body: (!body.is_empty()).then(|| body.to_owned()),
            }
        }
    }
}

fn clean_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn interface(e: &Element) -> Result<Interface, String> {
    let name = req_attr(e, "name")?.to_owned();
    let version = req_attr(e, "version")?.parse().map_err(|_| format!("{name}: bad version"))?;
    let requests = e.elements_named("request").map(message).collect::<Result<_, _>>()?;
    let events = e.elements_named("event").map(message).collect::<Result<_, _>>()?;
    let enums = e.elements_named("enum").map(enumeration).collect::<Result<_, _>>()?;
    Ok(Interface { name, version, doc: doc(e), requests, events, enums })
}

fn message(e: &Element) -> Result<Message, String> {
    let name = req_attr(e, "name")?.to_owned();
    let since = e.attr("since").map_or(Ok(1), str::parse).map_err(|_| format!("{name}: bad since"))?;
    let args = e.elements_named("arg").map(arg).collect::<Result<Vec<_>, _>>()?;
    if args.iter().filter(|a| a.ty == ArgType::NewId).count() > 1 {
        return Err(format!("{name}: more than one new_id argument is not supported"));
    }
    Ok(Message { name, destructor: e.attr("type") == Some("destructor"), since, doc: doc(e), args })
}

fn arg(e: &Element) -> Result<Arg, String> {
    let name = req_attr(e, "name")?.to_owned();
    let ty = match req_attr(e, "type")? {
        "int" => ArgType::Int,
        "uint" => ArgType::Uint,
        "fixed" => ArgType::Fixed,
        "string" => ArgType::String,
        "object" => ArgType::Object,
        "new_id" => ArgType::NewId,
        "array" => ArgType::Array,
        "fd" => ArgType::Fd,
        other => return Err(format!("{name}: unknown arg type `{other}`")),
    };
    Ok(Arg {
        name,
        ty,
        interface: e.attr("interface").map(str::to_owned),
        allow_null: e.attr("allow-null") == Some("true"),
        enum_ref: e.attr("enum").map(str::to_owned),
        summary: e.attr("summary").map(clean_line),
    })
}

fn enumeration(e: &Element) -> Result<Enum, String> {
    let name = req_attr(e, "name")?.to_owned();
    let entries = e
        .elements_named("entry")
        .map(|en| {
            let ename = req_attr(en, "name")?.to_owned();
            let raw = req_attr(en, "value")?;
            let value = match raw.strip_prefix("0x") {
                Some(hex) => u32::from_str_radix(hex, 16),
                None => raw.parse(),
            }
            .map_err(|_| format!("{name}.{ename}: bad value `{raw}`"))?;
            let summary = en.attr("summary").map(clean_line).or_else(|| doc(en).summary);
            Ok(Entry { name: ename, value, summary })
        })
        .collect::<Result<_, String>>()?;
    Ok(Enum { name, bitfield: e.attr("bitfield") == Some("true"), doc: doc(e), entries })
}

// ---------------------------------------------------------------------------
// Naming

fn camel(s: &str) -> String {
    s.split('_')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            let first = c.next().unwrap().to_ascii_uppercase();
            std::iter::once(first).chain(c).collect::<String>()
        })
        .collect()
}

fn ident(s: &str) -> String {
    const KEYWORDS: &[&str] = &[
        "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "do", "dyn", "else", "enum",
        "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "macro", "match", "mod",
        "move", "mut", "override", "priv", "pub", "ref", "return", "static", "struct", "trait", "true", "try", "type",
        "typeof", "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
    ];
    match s {
        "self" | "Self" | "super" | "crate" => format!("{s}_"),
        // Names the generated method bodies use themselves.
        "out" => "out_".to_owned(),
        _ if KEYWORDS.contains(&s) => format!("r#{s}"),
        _ => s.to_owned(),
    }
}

fn const_name(s: &str) -> String {
    let up = s.to_ascii_uppercase();
    if up.starts_with(|c: char| c.is_ascii_digit()) { format!("_{up}") } else { up }
}

// ---------------------------------------------------------------------------
// Code generation

/// Global knowledge needed to resolve cross-interface references.
struct Index {
    /// interface name -> handle type path
    interfaces: HashMap<String, String>,
    /// "iface.enum" -> type path
    enums: HashMap<String, String>,
}

impl Index {
    fn build(protocols: &[Protocol]) -> Index {
        let mut interfaces = HashMap::new();
        let mut enums = HashMap::new();
        for p in protocols {
            for i in &p.interfaces {
                interfaces.insert(i.name.clone(), format!("{ROOT}::{}::{}", i.name, camel(&i.name)));
                for e in &i.enums {
                    enums.insert(format!("{}.{}", i.name, e.name), format!("{ROOT}::{}::{}", i.name, camel(&e.name)));
                }
            }
        }
        Index { interfaces, enums }
    }

    fn handle(&self, iface: Option<&str>) -> Option<&str> {
        iface.and_then(|i| self.interfaces.get(i)).map(String::as_str)
    }

    fn enum_type(&self, owner: &str, r: &str) -> Option<&str> {
        let key = if r.contains('.') { r.to_owned() } else { format!("{owner}.{r}") };
        self.enums.get(&key).map(String::as_str)
    }
}

pub struct Output {
    /// (file name, contents)
    pub files: Vec<(String, String)>,
}

pub fn generate(protocols: &[Protocol]) -> Output {
    let index = Index::build(protocols);
    let mut files = Vec::new();
    for p in protocols {
        files.push((format!("{}.rs", p.name), gen_protocol(p, &index)));
    }
    files.push(("mod.rs".to_owned(), gen_mod(protocols)));
    Output { files }
}

fn header(src: &str) -> String {
    format!("// @generated by `cargo xtask gen-wayland` from protocols/{src}. Do not edit.\n\n")
}

fn write_doc(out: &mut String, indent: &str, doc: &Doc, extra: &[String]) {
    let mut lines: Vec<String> = Vec::new();
    if let Some(s) = &doc.summary {
        lines.push(s.clone());
    }
    if let Some(body) = &doc.body {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        // Strip indentation entirely so rustdoc never sees indented code blocks
        // (which would become doctests).
        let mut blank = false;
        for l in body.lines() {
            let t = l.trim();
            if t.is_empty() {
                if !blank {
                    lines.push(String::new());
                }
                blank = true;
            } else {
                lines.push(t.to_owned());
                blank = false;
            }
        }
    }
    for e in extra {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.extend(e.lines().map(str::to_owned));
    }
    for l in lines {
        if l.is_empty() {
            let _ = writeln!(out, "{indent}///");
        } else {
            let _ = writeln!(out, "{indent}/// {l}");
        }
    }
}

fn gen_protocol(p: &Protocol, index: &Index) -> String {
    let mut s = header(&p.file);
    for i in &p.interfaces {
        gen_interface(&mut s, i, index);
    }
    s
}

fn gen_interface(s: &mut String, i: &Interface, index: &Index) {
    let ty = camel(&i.name);
    write_doc(s, "", &i.doc, &[]);
    let _ = writeln!(s, "pub mod {} {{", i.name);
    let _ = writeln!(s, "    #[allow(unused_imports)]");
    let _ = writeln!(
        s,
        "    use crate::wayland::wire::{{self, Fixed, MessageReader, ObjectId, ObjectMap, Outgoing, Proxy, WireError}};"
    );
    let _ = writeln!(s, "    use {ROOT}::Interface;\n");

    // Handle type.
    write_doc(s, "    ", &i.doc, &[]);
    let _ = writeln!(s, "    #[derive(Clone, Copy, PartialEq, Eq, Hash)]");
    let _ = writeln!(s, "    pub struct {ty}(ObjectId);\n");
    let _ = writeln!(
        s,
        "    impl Proxy for {ty} {{\n        const INTERFACE: Interface = Interface::{ty};\n        fn from_id(id: ObjectId) -> Self {{ {ty}(id) }}\n        fn id(self) -> ObjectId {{ self.0 }}\n    }}\n"
    );
    let _ = writeln!(
        s,
        "    impl std::fmt::Debug for {ty} {{\n        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {{\n            write!(f, \"{}{{:?}}\", self.0)\n        }}\n    }}\n",
        i.name
    );

    // Requests.
    if !i.requests.is_empty() {
        let _ = writeln!(s, "    impl {ty} {{");
        for (opcode, r) in i.requests.iter().enumerate() {
            gen_request(s, i, r, opcode, index);
        }
        let _ = writeln!(s, "    }}\n");
    }

    // Events.
    gen_events(s, i, index);

    // Enums.
    for e in &i.enums {
        gen_enum(s, e);
    }

    let _ = writeln!(s, "}}\n");
}

fn request_param_type(i: &Interface, a: &Arg, index: &Index) -> String {
    let base = match a.ty {
        ArgType::Int | ArgType::Uint => match a.enum_ref.as_deref().and_then(|r| index.enum_type(&i.name, r)) {
            Some(t) => t.to_owned(),
            None => {
                if a.ty == ArgType::Int {
                    "i32".into()
                } else {
                    "u32".into()
                }
            }
        },
        ArgType::Fixed => "Fixed".into(),
        ArgType::String => "&str".into(),
        ArgType::Object => index.handle(a.interface.as_deref()).unwrap_or("ObjectId").to_owned(),
        ArgType::Array => "&[u8]".into(),
        ArgType::Fd => "std::os::fd::BorrowedFd<'_>".into(),
        ArgType::NewId => unreachable!("new_id is not a parameter"),
    };
    if a.allow_null && matches!(a.ty, ArgType::String | ArgType::Object) { format!("Option<{base}>") } else { base }
}

fn gen_request(s: &mut String, i: &Interface, r: &Message, opcode: usize, index: &Index) {
    let new_id = r.args.iter().find(|a| a.ty == ArgType::NewId);
    let generic = new_id.is_some_and(|a| a.interface.is_none());
    let has_fd = r.args.iter().any(|a| a.ty == ArgType::Fd);

    let mut extra = Vec::new();
    for a in &r.args {
        if a.ty == ArgType::NewId {
            continue;
        }
        if let Some(sum) = &a.summary {
            extra.push(format!("- `{}`: {sum}", a.name));
        }
    }
    let mut notes = Vec::new();
    if r.since > 1 {
        notes.push(format!("Since version {}.", r.since));
    }
    if r.destructor {
        notes.push("Destructor: the object must not be used after this request.".to_owned());
    }
    let mut extra_lines = Vec::new();
    if !extra.is_empty() {
        extra_lines.push(extra.join("\n"));
    }
    extra_lines.extend(notes);
    write_doc(s, "        ", &r.doc, &extra_lines);

    // Signature.
    let mut params = vec!["self".to_owned(), "out: &mut Outgoing".to_owned()];
    for a in &r.args {
        if a.ty == ArgType::NewId {
            if generic {
                params.push("version: u32".to_owned());
            }
            continue;
        }
        params.push(format!("{}: {}", ident(&a.name), request_param_type(i, a, index)));
    }
    let ret = match new_id {
        None => "()".to_owned(),
        Some(_) if generic => "P".to_owned(),
        Some(a) => index.handle(a.interface.as_deref()).unwrap_or("ObjectId").to_owned(),
    };
    let ret = if has_fd { format!("std::io::Result<{ret}>") } else { ret };
    let generics = if generic { "<P: Proxy>" } else { "" };
    let ret_sig = if ret == "()" { String::new() } else { format!(" -> {ret}") };
    let _ = writeln!(s, "        pub fn {}{generics}({}){ret_sig} {{", ident(&r.name), params.join(", "));

    // Body: dup fds and allocate ids before encoding starts.
    for a in r.args.iter().filter(|a| a.ty == ArgType::Fd) {
        let n = ident(&a.name);
        let _ = writeln!(s, "            let {n} = {n}.try_clone_to_owned()?;");
    }
    if let Some(a) = new_id {
        let n = ident(&a.name);
        let iface = if generic {
            "P::INTERFACE".to_owned()
        } else {
            match &a.interface {
                Some(name) if index.interfaces.contains_key(name) => format!("Interface::{}", camel(name)),
                _ => panic!("{}.{}: new_id of unknown interface", i.name, r.name),
            }
        };
        let _ = writeln!(s, "            let {n} = out.objects.alloc({iface});");
    }
    let _ = write!(s, "            out.begin(self.0, {opcode})");
    for a in &r.args {
        let n = ident(&a.name);
        let typed_obj = index.handle(a.interface.as_deref()).is_some();
        let has_enum = a.enum_ref.as_deref().and_then(|e| index.enum_type(&i.name, e)).is_some();
        let call = match a.ty {
            ArgType::Int if has_enum => format!(".int({n}.0 as i32)"),
            ArgType::Uint if has_enum => format!(".uint({n}.0)"),
            ArgType::Int => format!(".int({n})"),
            ArgType::Uint => format!(".uint({n})"),
            ArgType::Fixed => format!(".fixed({n})"),
            ArgType::String if a.allow_null => format!(".string({n})"),
            ArgType::String => format!(".string(Some({n}))"),
            ArgType::Object => match (a.allow_null, typed_obj) {
                (true, true) => format!(".object({n}.map(Proxy::id))"),
                (true, false) => format!(".object({n})"),
                (false, true) => format!(".object(Some({n}.id()))"),
                (false, false) => format!(".object(Some({n}))"),
            },
            ArgType::NewId if generic => format!(".string(Some(P::INTERFACE.name())).uint(version).new_id({n})"),
            ArgType::NewId => format!(".new_id({n})"),
            ArgType::Array => format!(".array({n})"),
            ArgType::Fd => format!(".fd({n})"),
        };
        s.push_str(&call);
    }
    let _ = writeln!(s, ".finish();");
    if r.destructor {
        let _ = writeln!(s, "            out.destroyed(self.0);");
    }
    let value = match new_id {
        None => "()".to_owned(),
        Some(a) if generic => format!("P::from_id({})", ident(&a.name)),
        Some(a) => format!("{}::from_id({})", index.handle(a.interface.as_deref()).unwrap(), ident(&a.name)),
    };
    match (has_fd, value.as_str()) {
        (true, v) => {
            let _ = writeln!(s, "            Ok({v})");
        }
        (false, "()") => {}
        (false, v) => {
            let _ = writeln!(s, "            {v}");
        }
    }
    let _ = writeln!(s, "        }}\n");
}

fn event_field_type(i: &Interface, a: &Arg, index: &Index) -> String {
    let base = match a.ty {
        ArgType::Int | ArgType::Uint => match a.enum_ref.as_deref().and_then(|r| index.enum_type(&i.name, r)) {
            Some(t) => t.to_owned(),
            None => {
                if a.ty == ArgType::Int {
                    "i32".into()
                } else {
                    "u32".into()
                }
            }
        },
        ArgType::Fixed => "Fixed".into(),
        ArgType::String => "String".into(),
        ArgType::Object | ArgType::NewId => index.handle(a.interface.as_deref()).unwrap_or("ObjectId").to_owned(),
        ArgType::Array => "Vec<u8>".into(),
        ArgType::Fd => "std::os::fd::OwnedFd".into(),
    };
    if a.allow_null && matches!(a.ty, ArgType::String | ArgType::Object) { format!("Option<{base}>") } else { base }
}

fn gen_events(s: &mut String, i: &Interface, index: &Index) {
    let ty = camel(&i.name);
    let _ = writeln!(s, "    /// Events sent by the compositor to a [`{ty}`].");
    let _ = writeln!(s, "    #[derive(Debug)]");
    let _ = writeln!(s, "    pub enum Event {{");
    for e in &i.events {
        let mut notes = Vec::new();
        if e.since > 1 {
            notes.push(format!("Since version {}.", e.since));
        }
        write_doc(s, "        ", &e.doc, &notes);
        if e.args.is_empty() {
            let _ = writeln!(s, "        {},", camel(&e.name));
        } else {
            let _ = writeln!(s, "        {} {{", camel(&e.name));
            for a in &e.args {
                if let Some(sum) = &a.summary {
                    let _ = writeln!(s, "            /// {sum}");
                }
                let _ = writeln!(s, "            {}: {},", ident(&a.name), event_field_type(i, a, index));
            }
            let _ = writeln!(s, "        }},");
        }
    }
    let _ = writeln!(s, "    }}\n");

    let _ = writeln!(s, "    impl Event {{");
    let _ = writeln!(s, "        #[allow(unused_variables)]");
    let _ = writeln!(
        s,
        "        pub fn parse(opcode: u16, r: &mut MessageReader<'_>, objects: &mut ObjectMap) -> Result<Event, WireError> {{"
    );
    let _ = writeln!(s, "            match opcode {{");
    for (opcode, e) in i.events.iter().enumerate() {
        let _ = writeln!(s, "                {opcode} => {{");
        for a in &e.args {
            let n = ident(&a.name);
            let handle = index.handle(a.interface.as_deref());
            let enum_ty = a.enum_ref.as_deref().and_then(|r| index.enum_type(&i.name, r));
            let expr = match a.ty {
                ArgType::Int => match enum_ty {
                    Some(t) => format!("{t}(r.int()? as u32)"),
                    None => "r.int()?".to_owned(),
                },
                ArgType::Uint => match enum_ty {
                    Some(t) => format!("{t}(r.uint()?)"),
                    None => "r.uint()?".to_owned(),
                },
                ArgType::Fixed => "r.fixed()?".to_owned(),
                ArgType::String if a.allow_null => "r.string()?".to_owned(),
                ArgType::String => "wire::non_null(r.string()?)?".to_owned(),
                ArgType::Object => match (a.allow_null, handle) {
                    (true, Some(h)) => format!("r.object()?.map({h}::from_id)"),
                    (true, None) => "r.object()?".to_owned(),
                    (false, Some(h)) => format!("{h}::from_id(wire::non_null(r.object()?)?)"),
                    (false, None) => "wire::non_null(r.object()?)?".to_owned(),
                },
                ArgType::NewId => match (a.interface.as_deref(), handle) {
                    (Some(iface), Some(h)) => format!(
                        "{{ let id = r.new_id()?; objects.insert_server(id, Interface::{})?; {h}::from_id(id) }}",
                        camel(iface)
                    ),
                    _ => "r.new_id()?".to_owned(),
                },
                ArgType::Array => "r.array()?".to_owned(),
                ArgType::Fd => "r.fd()?".to_owned(),
            };
            let _ = writeln!(s, "                    let {n} = {expr};");
        }
        let fields: Vec<String> = e.args.iter().map(|a| ident(&a.name)).collect();
        if fields.is_empty() {
            let _ = writeln!(s, "                    Ok(Event::{})", camel(&e.name));
        } else {
            let _ = writeln!(s, "                    Ok(Event::{} {{ {} }})", camel(&e.name), fields.join(", "));
        }
        let _ = writeln!(s, "                }}");
    }
    let _ = writeln!(s, "                _ => Err(WireError::UnknownOpcode {{ interface: Interface::{ty}, opcode }}),");
    let _ = writeln!(s, "            }}\n        }}\n    }}\n");

    // Destructor events (e.g. wl_callback.done) are noted so the app can drop the id.
    let destructors: Vec<usize> = i.events.iter().enumerate().filter(|(_, e)| e.destructor).map(|(n, _)| n).collect();
    let _ = writeln!(s, "    /// Opcodes of events after which the object is destroyed by the compositor.");
    let _ = writeln!(s, "    pub const DESTRUCTOR_EVENTS: &[u16] = &{destructors:?};\n");
    // Names by opcode, for traces.
    let names = |m: &[Message]| m.iter().map(|m| format!("{:?}", m.name)).collect::<Vec<_>>().join(", ");
    let _ = writeln!(s, "    /// Request names, by opcode.");
    let _ = writeln!(s, "    pub const REQUESTS: &[&str] = &[{}];\n", names(&i.requests));
    let _ = writeln!(s, "    /// Event names, by opcode.");
    let _ = writeln!(s, "    pub const EVENTS: &[&str] = &[{}];\n", names(&i.events));
}

fn gen_enum(s: &mut String, e: &Enum) {
    let ty = camel(&e.name);
    write_doc(s, "    ", &e.doc, &[]);
    let derives =
        if e.bitfield { "Clone, Copy, PartialEq, Eq, Hash, Default" } else { "Clone, Copy, PartialEq, Eq, Hash" };
    let _ = writeln!(s, "    #[derive({derives})]");
    let _ = writeln!(s, "    pub struct {ty}(pub u32);\n");
    let _ = writeln!(s, "    impl {ty} {{");
    for en in &e.entries {
        if let Some(sum) = &en.summary {
            let _ = writeln!(s, "        /// {sum}");
        }
        let _ = writeln!(s, "        pub const {}: {ty} = {ty}({});", const_name(&en.name), en.value);
    }
    if e.bitfield {
        let _ = writeln!(s, "\n        pub fn contains(self, other: {ty}) -> bool {{ self.0 & other.0 == other.0 }}");
    }
    let _ = writeln!(s, "    }}\n");

    // First name wins for aliased values.
    let mut by_value: BTreeMap<u32, &str> = BTreeMap::new();
    for en in &e.entries {
        by_value.entry(en.value).or_insert(&en.name);
    }
    let _ = writeln!(s, "    impl std::fmt::Debug for {ty} {{");
    let _ = writeln!(s, "        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {{");
    if e.bitfield {
        let flags: Vec<String> =
            by_value.iter().filter(|(v, _)| **v != 0).map(|(v, n)| format!("({v}, \"{}\")", const_name(n))).collect();
        let _ = writeln!(s, "            const FLAGS: &[(u32, &str)] = &[{}];", flags.join(", "));
        let _ = writeln!(s, "            let mut rest = self.0;");
        let _ = writeln!(s, "            let mut first = true;");
        let _ = writeln!(s, "            write!(f, \"{ty}(\")?;");
        let _ = writeln!(s, "            for &(bit, name) in FLAGS {{");
        let _ = writeln!(s, "                if bit & rest == bit {{");
        let _ = writeln!(s, "                    if !first {{ f.write_str(\" | \")?; }}");
        let _ = writeln!(s, "                    f.write_str(name)?;");
        let _ = writeln!(s, "                    rest &= !bit;");
        let _ = writeln!(s, "                    first = false;");
        let _ = writeln!(s, "                }}");
        let _ = writeln!(s, "            }}");
        let _ = writeln!(s, "            if rest != 0 || first {{");
        let _ = writeln!(s, "                if !first {{ f.write_str(\" | \")?; }}");
        let _ = writeln!(s, "                write!(f, \"{{rest:#x}}\")?;");
        let _ = writeln!(s, "            }}");
        let _ = writeln!(s, "            f.write_str(\")\")");
    } else {
        let _ = writeln!(s, "            match self.0 {{");
        for (v, n) in &by_value {
            let _ = writeln!(s, "                {v} => f.write_str(\"{}\"),", const_name(n));
        }
        let _ = writeln!(s, "                v => write!(f, \"{ty}({{v}})\"),");
        let _ = writeln!(s, "            }}");
    }
    let _ = writeln!(s, "        }}\n    }}\n");

    if e.bitfield {
        for (tr, method, op) in [("BitOr", "bitor", "|"), ("BitAnd", "bitand", "&")] {
            let _ = writeln!(
                s,
                "    impl std::ops::{tr} for {ty} {{\n        type Output = {ty};\n        fn {method}(self, rhs: {ty}) -> {ty} {{ {ty}(self.0 {op} rhs.0) }}\n    }}\n"
            );
        }
    }
}

fn gen_mod(protocols: &[Protocol]) -> String {
    let mut s = String::from(
        "// @generated by `cargo xtask gen-wayland`. Do not edit.\n\n//! Typed bindings for the Wayland protocols in `protocols/`.\n\n",
    );
    for p in protocols {
        let _ = writeln!(
            s,
            "#[allow(clippy::all, missing_docs, rustdoc::broken_intra_doc_links, rustdoc::bare_urls)]\npub mod {};",
            p.name
        );
    }
    s.push('\n');
    for p in protocols {
        let names: Vec<&str> = p.interfaces.iter().map(|i| i.name.as_str()).collect();
        let _ = writeln!(s, "pub use self::{}::{{{}}};", p.name, names.join(", "));
    }

    let all: Vec<&Interface> = protocols.iter().flat_map(|p| &p.interfaces).collect();

    s.push_str("\nuse crate::wayland::wire::{MessageReader, ObjectId, ObjectMap, Proxy, WireError};\n\n");
    s.push_str("/// Every interface known to this build.\n");
    s.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\npub enum Interface {\n");
    for i in &all {
        let _ = writeln!(s, "    {},", camel(&i.name));
    }
    s.push_str("}\n\nimpl Interface {\n");
    let _ = writeln!(
        s,
        "    pub const ALL: &[Interface] = &[{}];\n",
        all.iter().map(|i| format!("Interface::{}", camel(&i.name))).collect::<Vec<_>>().join(", ")
    );
    s.push_str("    pub fn name(self) -> &'static str {\n        match self {\n");
    for i in &all {
        let _ = writeln!(s, "            Interface::{} => \"{}\",", camel(&i.name), i.name);
    }
    s.push_str("        }\n    }\n\n");
    s.push_str(
        "    /// Highest version this build implements.\n    pub fn version(self) -> u32 {\n        match self {\n",
    );
    for i in &all {
        let _ = writeln!(s, "            Interface::{} => {},", camel(&i.name), i.version);
    }
    s.push_str("        }\n    }\n\n");
    s.push_str("    pub fn from_name(name: &str) -> Option<Interface> {\n        Interface::ALL.iter().copied().find(|i| i.name() == name)\n    }\n\n");
    s.push_str("    /// Whether `opcode` is a destructor event for this interface.\n    pub fn is_destructor_event(self, opcode: u16) -> bool {\n        match self {\n");
    for i in &all {
        let _ = writeln!(
            s,
            "            Interface::{} => {}::DESTRUCTOR_EVENTS.contains(&opcode),",
            camel(&i.name),
            i.name
        );
    }
    s.push_str("        }\n    }\n\n");
    for (what, table) in [("request", "REQUESTS"), ("event", "EVENTS")] {
        let _ = write!(
            s,
            "    /// The name of {what} `opcode`, if there is one.\n    pub fn {what}_name(self, opcode: u16) -> Option<&'static str> {{\n        match self {{\n"
        );
        for i in &all {
            let _ = writeln!(
                s,
                "            Interface::{} => {}::{table}.get(opcode as usize).copied(),",
                camel(&i.name),
                i.name
            );
        }
        s.push_str("        }\n    }\n\n");
    }
    s.truncate(s.len() - 1);
    s.push_str("}\n\n");

    s.push_str("/// An event from any object, tagged with its typed sender.\n#[derive(Debug)]\npub enum AnyEvent {\n");
    for i in &all {
        let _ = writeln!(s, "    {0}({1}::{0}, {1}::Event),", camel(&i.name), i.name);
    }
    s.push_str("}\n\nimpl AnyEvent {\n");
    s.push_str("    /// Decode an event's arguments. `objects` resolves the sender's interface and\n    /// receives any objects the event creates.\n");
    s.push_str("    pub fn parse(sender: ObjectId, opcode: u16, r: &mut MessageReader<'_>, objects: &mut ObjectMap) -> Result<AnyEvent, WireError> {\n");
    s.push_str("        let interface = objects.get(sender).ok_or(WireError::UnknownObject(sender))?;\n        Ok(match interface {\n");
    for i in &all {
        let _ = writeln!(
            s,
            "            Interface::{0} => AnyEvent::{0}({1}::{0}::from_id(sender), {1}::Event::parse(opcode, r, objects)?),",
            camel(&i.name),
            i.name
        );
    }
    s.push_str("        })\n    }\n}\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn naming() {
        assert_eq!(camel("zwp_linux_dmabuf_v1"), "ZwpLinuxDmabufV1");
        assert_eq!(ident("move"), "r#move");
        assert_eq!(ident("self"), "self_");
        assert_eq!(const_name("90"), "_90");
        assert_eq!(const_name("argb8888"), "ARGB8888");
    }
}
