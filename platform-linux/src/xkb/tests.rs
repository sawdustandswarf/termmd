//! Differential tests against libxkbcommon's answers (see data/xkb/gen_reference.py).

use super::{Keymap, Keysym};

const FIXTURES: &[(&str, &str, &str)] = &[
    ("us-omarchy", include_str!("../../../data/xkb/us-omarchy.xkb"), include_str!("../../../data/xkb/us-omarchy.ref")),
    ("de", include_str!("../../../data/xkb/de.xkb"), include_str!("../../../data/xkb/de.ref")),
    ("fr", include_str!("../../../data/xkb/fr.xkb"), include_str!("../../../data/xkb/fr.ref")),
    ("gb-altgr", include_str!("../../../data/xkb/gb-altgr.xkb"), include_str!("../../../data/xkb/gb-altgr.ref")),
    ("us-ru", include_str!("../../../data/xkb/us-ru.xkb"), include_str!("../../../data/xkb/us-ru.ref")),
];

#[test]
fn matches_libxkbcommon() {
    let mut failures = Vec::new();
    let mut checked = 0;
    for (name, src, reference) in FIXTURES {
        let keymap = Keymap::parse(src).unwrap_or_else(|e| panic!("{name}: {e}"));
        for line in reference.lines().filter(|l| !l.starts_with('#')) {
            let f: Vec<&str> = line.split('\t').collect();
            match f[0] {
                "R" => {
                    let kc: u32 = f[1].parse().unwrap();
                    let want = f[2] == "1";
                    if keymap.repeats(kc) != want {
                        failures.push(format!("{name}: key {kc} <{:?}> repeats={want} expected", keymap.key_name(kc)));
                    }
                }
                "K" => {
                    let kc: u32 = f[1].parse().unwrap();
                    let group: u32 = f[2].parse().unwrap();
                    let mods: u32 = f[3].parse().unwrap();
                    let level: usize = f[4].parse().unwrap();
                    let consumed: u32 = f[5].parse().unwrap();
                    let syms: Vec<Keysym> = if f.len() > 6 && !f[6].is_empty() {
                        f[6].split(',').map(|s| Keysym(u32::from_str_radix(s, 16).unwrap())).collect()
                    } else {
                        Vec::new()
                    };
                    checked += 1;
                    let got = keymap.lookup(kc, mods, group);
                    let (g_syms, g_level, g_consumed) =
                        got.as_ref().map_or((&[][..], 0, 0), |l| (&*l.keysyms, l.level, l.consumed));
                    if g_syms != syms.as_slice() || g_level != level || g_consumed != consumed {
                        failures.push(format!(
                            "{name}: key {kc} <{}> type {:?} group {group} mods {mods:#x}: \
                             want {syms:?} L{level} c{consumed:#x}, got {g_syms:?} L{g_level} c{g_consumed:#x}",
                            keymap.key_name(kc).unwrap_or("?"),
                            keymap.key_type_name(kc, group as usize),
                        ));
                    }
                }
                _ => panic!("bad reference line {line}"),
            }
        }
    }
    if !failures.is_empty() {
        let n = failures.len();
        failures.truncate(25);
        panic!("{n} mismatches out of {checked} lookups, first ones:\n{}", failures.join("\n"));
    }
}

#[test]
fn named_modifiers_resolve() {
    let keymap = Keymap::parse(FIXTURES[0].1).unwrap();
    assert_eq!(keymap.mod_mask("Shift"), 1);
    assert_eq!(keymap.mod_mask("Control"), 4);
    assert_eq!(keymap.mod_mask("Alt"), 1 << 3, "Alt -> Mod1");
    assert_eq!(keymap.mod_mask("Super"), 1 << 6, "Super -> Mod4");
    assert_eq!(keymap.mod_mask("NumLock"), 1 << 4, "NumLock -> Mod2");
    assert_eq!(keymap.mod_mask("LevelThree"), 1 << 7, "LevelThree -> Mod5");
}
