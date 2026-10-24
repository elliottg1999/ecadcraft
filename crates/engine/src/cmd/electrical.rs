//! Electrical design: IEC 81346 device tags on schematic symbols.
//!
//! A *device* is a block reference that carries a tag attribute (`TAG` by default). ETAG gives
//! every device a reference designation from its IEC 81346-2 class letters and where it sits on
//! the schematic: the sheet and column of a frame grid, EPLAN-style (`-K2.4` is the relay in
//! column 4 of sheet 2). EDEVICES lists the devices with their tags and reports duplicates.
//!
//! Sheets lie side by side in the current space, left to right from `origin`, each `sheetWidth`
//! wide and divided into `columns` equal columns.
//!
//! The class of a device comes from, in order: the `classes` parameter (block name → letters),
//! the letters of the tag it already has (`-K`, `-K?`, `-QA3`), or a word in its block's name
//! (`RELAY_24V` → `K`, see [`classes`]).

mod builder;
mod check;
mod classes;
mod conductors;
mod demo;
pub mod generate;
mod interactive;
mod manager;
mod parts;
mod pins;
mod plc;
mod project;
mod reports;
mod symbols;
mod sync;
mod terminals;
mod titleblock;
mod wires;
mod wiring;
mod xref;

use std::collections::{BTreeMap, HashMap, HashSet};

use cadcraft_doc::{EntityKind, Handle};
use serde_json::{Map, Value, json};

use super::curves::KW;
use super::{CommandSpec, bad, bool_or, f64_or, point_param, str_param};
use crate::{EngineError, Input, Interactive, Prompt, Result, Session, Step};

use classes::{class_from_block_name, class_from_tag, contact_kind, is_auxiliary, is_numbered};

/// Attributes that hold a device's tag, AutoCAD Electrical style: `TAG1` on a main device (a
/// coil, a motor), `TAG2` on a child (a contact, carrying its main device's tag), plain `TAG` too.
const TAG_ATTRIBUTES: &[&str] = &["TAG1", "TAG2", "TAG"];
/// The attribute of a child component.
const CHILD_ATTRIBUTE: &str = "TAG2";
/// Default tag format.
const FORMAT: &str = "-{class}{sheet}{col2}";
/// Longest tag format and resulting tag accepted (characters).
const MAX_FORMAT: usize = 64;
const MAX_TAG: usize = 255;
/// Highest sheet number and column count accepted.
const MAX_SHEET: f64 = 99_999.0;
const MAX_COLUMNS: u64 = 1_000;
/// Most devices tagged in one go (and most suffixes tried for one tag).
const MAX_DEVICES: usize = 200_000;

/// Run after every command that changed the drawing (the engine's after-command hook): keeps
/// the project database and the cross-references in step with ordinary CAD edits.
pub(crate) fn after_command(s: &mut Session, before: Option<&std::sync::Arc<cadcraft_doc::Drawing>>, uid: Option<u64>) {
    retag_copies(s, before, uid);
    xref::live(s, before, uid);
}

/// Copies keep their tag in AutoCAD Electrical only for contacts: a main device that appeared in
/// this command (COPY, ARRAY, paste…) with the numbered tag of another device gets a new tag from
/// where it now is.
fn retag_copies(s: &mut Session, before: Option<&std::sync::Arc<cadcraft_doc::Drawing>>, uid: Option<u64>) {
    let (Some(before), Ok(st)) = (before, s.state()) else { return };
    if uid.is_some_and(|u| u != st.uid) || std::sync::Arc::ptr_eq(before, &st.doc) {
        return;
    }
    let space = s.space();
    let existed: HashSet<Handle> = before.space(&space).map(|st| st.iter().map(|e| e.handle).collect()).unwrap_or_default();
    let Ok(all) = devices(s, None, None) else { return };
    let overrides = HashMap::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for d in all.iter().filter(|d| !is_child(d, &overrides) && d.family.as_deref() != Some("TERMINAL") && is_numbered(&d.tag)) {
        *seen.entry(d.tag.clone()).or_default() += 1;
    }
    // A copy carries the database id (EID) of the device it was copied from; a device inserted
    // with a duplicate tag on purpose has none, and keeps its tag (EDEVICES reports it).
    let eids: HashMap<Handle, String> = st
        .doc
        .space(&space)
        .map(|store| {
            store
                .iter()
                .filter_map(|e| match &e.kind {
                    EntityKind::Insert(i) => i
                        .attribs
                        .iter()
                        .find(|a| a.tag.eq_ignore_ascii_case("EID"))
                        .filter(|a| !a.text.value.is_empty())
                        .map(|a| (e.handle, a.text.value.clone())),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let copied = |h: &Handle| eids.get(h).is_some_and(|id| eids.iter().any(|(o, x)| o != h && existed.contains(o) && x == id));
    let fresh: Vec<String> = all
        .iter()
        .filter(|d| !existed.contains(&d.handle) && copied(&d.handle))
        .filter(|d| !is_child(d, &overrides) && d.family.as_deref() != Some("TERMINAL"))
        .filter(|d| seen.get(&d.tag).is_some_and(|n| *n > 1))
        .map(|d| d.handle.hex())
        .collect();
    if fresh.is_empty() {
        return;
    }
    // Clear their tags, then tag them like new devices (mode new keeps every other tag).
    for d in all.iter().filter(|d| fresh.contains(&d.handle.hex())) {
        let _ = set_tag(s, d.handle, &d.attr, "");
    }
    if let Err(e) = run_etag_inner(s, &json!({ "handles": fresh })) {
        log::warn!("copied devices not retagged: {e}");
    }
}

pub fn specs() -> Vec<CommandSpec> {
    let mut v = vec![
        CommandSpec::new("etag", "Tag Devices (IEC 81346)", run_etag)
            .menu(&["Tools", "Electrical", "Tag Devices"])
            .params(
                "{mode?: \"new\" (default: only devices without a numbered tag) | \"all\" (renumber every device), \
                 handles? (default: every device in the current space), format? (default \"-{class}{sheet}{col2}\", e.g. -K105; \
                 placeholders {class} {sheet} {column} {col2} (two digits) {count}), origin? [x, y] (default [0, 0]), sheetWidth? (default 420), \
                 columns? (default 10), firstSheet? (default 1), function? (\"=\" aspect), location? (\"+\" aspect), \
                 attribute? (default: TAG1, TAG2 or TAG), classes? {blockName: \"K\"}, dryRun? bool} \
                 → {tagged, kept, devices: [{handle, block, old, tag}], unclassified: [handle], outside: [handle]}",
            )
            .interactive(|_| Ok(Box::new(EtagM))),
        CommandSpec::new("edevices", "List Devices (IEC 81346)", run_edevices)
            .menu(&["Tools", "Electrical", "List Devices"])
            .params(
                "{attribute? (default: TAG1, TAG2 or TAG), origin?, sheetWidth?, columns?, firstSheet?} \
                 → {devices: [{handle, block, tag, class, sheet, column}], duplicates: {tag: [handle]}, untagged}",
            )
            .noundo(),
    ];
    v.extend(wires::specs());
    v.extend(xref::specs());
    v.extend(symbols::specs());
    v.extend(wiring::specs());
    v.extend(builder::specs());
    v.extend(generate::specs());
    v.extend(terminals::specs());
    v.extend(plc::specs());
    v.extend(parts::specs());
    v.extend(demo::specs());
    v.extend(reports::specs());
    v.extend(conductors::specs());
    v.extend(titleblock::specs());
    v.extend(project::specs());
    v.extend(sync::specs());
    v.extend(check::specs());
    v.extend(manager::specs());
    v
}

/// The sheet grid devices are placed on.
#[derive(Clone, Copy, Debug)]
struct Grid {
    origin_x: f64,
    sheet_width: f64,
    columns: u64,
    first_sheet: u64,
}

impl Grid {
    fn from_params(cmd: &str, p: &Value) -> Result<Grid> {
        let origin_x = point_param(p, "origin").map(|o| o.x).unwrap_or(0.0);
        let sheet_width = f64_or(p, "sheetWidth", 420.0);
        if sheet_width <= 0.0 {
            return Err(bad(cmd, "`sheetWidth` must be positive"));
        }
        let columns = match p.get("columns") {
            None | Some(Value::Null) => 10,
            Some(v) => v
                .as_u64()
                .filter(|c| (1..=MAX_COLUMNS).contains(c))
                .ok_or_else(|| bad(cmd, format!("`columns` must be a whole number from 1 to {MAX_COLUMNS}")))?,
        };
        let first_sheet = match p.get("firstSheet") {
            None | Some(Value::Null) => 1,
            Some(v) => v.as_u64().filter(|c| *c as f64 <= MAX_SHEET).ok_or_else(|| bad(cmd, "`firstSheet` must be a whole number, 0 or more"))?,
        };
        Ok(Grid { origin_x, sheet_width, columns, first_sheet })
    }

    /// The sheet and column (both from 1, the sheet from `first_sheet`) at `x`, or `None` left of
    /// the first sheet or beyond the last sheet number.
    fn locate(&self, x: f64) -> Option<(u64, u64)> {
        let rel = x - self.origin_x;
        if !rel.is_finite() || rel < 0.0 {
            return None;
        }
        let idx = (rel / self.sheet_width).floor();
        if !idx.is_finite() || idx + self.first_sheet as f64 > MAX_SHEET {
            return None;
        }
        let within = rel - idx * self.sheet_width;
        let col_width = self.sheet_width / self.columns as f64;
        let col = ((within / col_width).floor().max(0.0) as u64).saturating_add(1).min(self.columns);
        Some((self.first_sheet.saturating_add(idx as u64), col))
    }
}

/// A block reference with a tag attribute.
#[derive(Clone, Debug)]
struct Device {
    handle: Handle,
    block: String,
    /// The attribute holding the tag (`TAG1`, `TAG2`, …) and its value.
    attr: String,
    tag: String,
    /// Class letters and family from the symbol's `ECLASS` / `EFAMILY` attributes.
    eclass: Option<String>,
    family: Option<String>,
    x: f64,
    y: f64,
}

/// The devices among `handles` (every object in the current space when `None`).
/// The tag attribute named in the `attribute` parameter, if any.
fn attribute_param(p: &Value) -> Option<String> {
    str_param(p, "attribute").map(str::trim).filter(|a| !a.is_empty()).map(str::to_string)
}

/// The attribute of `ins` holding its tag: `attribute` when given, else the first of
/// [`TAG_ATTRIBUTES`] it has.
fn tag_attrib<'a>(ins: &'a cadcraft_doc::Insert, attribute: Option<&str>) -> Option<&'a cadcraft_doc::Attrib> {
    match attribute {
        Some(a) => ins.attribs.iter().find(|x| x.tag.eq_ignore_ascii_case(a)),
        None => TAG_ATTRIBUTES.iter().find_map(|t| ins.attribs.iter().find(|x| x.tag.eq_ignore_ascii_case(t))),
    }
}

/// The non-empty value of attribute `tag` of `ins`.
fn attr_value(ins: &cadcraft_doc::Insert, tag: &str) -> Option<String> {
    ins.attribs.iter().find(|a| a.tag.eq_ignore_ascii_case(tag)).map(|a| a.text.value.trim().to_string()).filter(|v| !v.is_empty())
}

fn devices(s: &Session, attribute: Option<&str>, handles: Option<&[Handle]>) -> Result<Vec<Device>> {
    let d = s.doc()?;
    let Some(store) = d.space(&s.space()) else { return Ok(Vec::new()) };
    let wanted: Option<HashSet<Handle>> = handles.map(|h| h.iter().copied().collect());
    let mut out = Vec::new();
    for e in store.iter() {
        if wanted.as_ref().is_some_and(|w| !w.contains(&e.handle)) {
            continue;
        }
        let EntityKind::Insert(ins) = &e.kind else { continue };
        let Some(a) = tag_attrib(ins, attribute) else { continue };
        if out.len() >= MAX_DEVICES {
            return Err(EngineError::Other(format!("More than {MAX_DEVICES} devices; tag a selection at a time.")));
        }
        out.push(Device {
            handle: e.handle,
            block: ins.block.clone(),
            attr: a.tag.clone(),
            tag: a.text.value.trim().to_string(),
            eclass: attr_value(ins, "ECLASS").map(|c| c.to_ascii_uppercase()).filter(|c| valid_class(c)),
            family: attr_value(ins, "EFAMILY").map(|f| f.to_ascii_uppercase()),
            x: ins.insert.x,
            y: ins.insert.y,
        });
    }
    Ok(out)
}

/// Class letters given in the `classes` parameter: 1–3 capital letters, the first a main class.
fn valid_class(c: &str) -> bool {
    let n = c.chars().count();
    (1..=3).contains(&n) && c.chars().all(|ch| ch.is_ascii_uppercase()) && c.chars().next().is_some_and(classes::is_main_class)
}

fn class_overrides(cmd: &str, p: &Value) -> Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    match p.get("classes") {
        None | Some(Value::Null) => {}
        Some(Value::Object(m)) => {
            for (k, v) in m {
                let c = v
                    .as_str()
                    .map(|c| c.trim().to_ascii_uppercase())
                    .filter(|c| valid_class(c))
                    .ok_or_else(|| bad(cmd, format!("`classes.{k}` must be IEC 81346-2 class letters such as \"K\" or \"QA\"")))?;
                out.insert(k.to_ascii_lowercase(), c);
            }
        }
        Some(_) => return Err(bad(cmd, "`classes` must be an object mapping block names to class letters")),
    }
    Ok(out)
}

/// The class of `dev`: an override for its block, its tag's letters, or its block name's.
fn class_of(dev: &Device, overrides: &HashMap<String, String>) -> Option<String> {
    overrides
        .get(&dev.block.to_ascii_lowercase())
        .cloned()
        .or_else(|| dev.eclass.clone())
        .or_else(|| class_from_tag(&dev.tag))
        .or_else(|| class_from_block_name(&dev.block).map(str::to_string))
}

/// `format` with its placeholders filled.
fn render(format: &str, class: &str, sheet: u64, column: u64, count: Option<u64>) -> String {
    let mut t = format
        .replace("{class}", class)
        .replace("{sheet}", &sheet.to_string())
        .replace("{col2}", &format!("{column:02}"))
        .replace("{column}", &column.to_string());
    if let Some(n) = count {
        t = t.replace("{count}", &n.to_string());
    }
    t
}

/// The `=function+location` prefix of a tag.
fn aspects(p: &Value) -> String {
    let mut out = String::new();
    if let Some(f) = str_param(p, "function").map(str::trim).filter(|f| !f.is_empty()) {
        out.push('=');
        out.push_str(f.trim_start_matches('='));
    }
    if let Some(l) = str_param(p, "location").map(str::trim).filter(|l| !l.is_empty()) {
        out.push('+');
        out.push_str(l.trim_start_matches('+'));
    }
    out
}

/// The first of `base`, `base.2`, `base.3`, … not in `used`.
fn unique(base: &str, used: &HashSet<String>) -> Option<String> {
    if !used.contains(base) {
        return Some(base.to_string());
    }
    (1..MAX_DEVICES as u64 + 1).map(|n| format!("{base}{}", letters(n))).find(|t| !used.contains(t))
}

/// Suffix `n` (from 1) in letters, as AutoCAD Electrical marks a second reference in a column:
/// A, B, … Z, AA, AB, …
fn letters(mut n: u64) -> String {
    let mut out = Vec::new();
    while n > 0 {
        n -= 1;
        out.push(char::from(b'A' + (n % 26) as u8));
        n /= 26;
    }
    out.iter().rev().collect()
}

fn handle_list(cmd: &str, p: &Value) -> Result<Option<Vec<Handle>>> {
    match p.get("handles") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_str()
                    .and_then(Handle::parse_hex)
                    .or_else(|| v.as_u64().map(Handle))
                    .ok_or_else(|| bad(cmd, format!("`handles`: {v} is not an object handle")))
            })
            .collect::<Result<Vec<_>>>()
            .map(Some),
        Some(_) => Err(bad(cmd, "`handles` must be an array of object handles")),
    }
}

/// Whether `d` is a child component (a contact) carrying its main device's tag: it has a `TAG2`
/// attribute, or its block's name says so and no class override makes it a main device.
fn is_child(d: &Device, overrides: &HashMap<String, String>) -> bool {
    d.attr.eq_ignore_ascii_case(CHILD_ATTRIBUTE) || (is_auxiliary(&d.block) && !overrides.contains_key(&d.block.to_ascii_lowercase()))
}

/// `NO` or `NC` for a contact: its `EFAMILY`, else what its block's name says.
fn kind_of(d: &Device) -> &'static str {
    match d.family.as_deref() {
        // Groups of a PLC module or a drive placed apart: a plain page reference.
        Some("PLC" | "VFD") => "",
        Some("NC") => "NC",
        Some("NO") => "NO",
        _ => contact_kind(&d.block),
    }
}

/// Set the `attribute` value of block reference `h` to `tag`.
fn set_tag(s: &mut Session, h: Handle, attribute: &str, tag: &str) -> Result<()> {
    s.doc_mut()?.modify_entity(h, |e| {
        if let EntityKind::Insert(ins) = &mut e.kind
            && let Some(a) = ins.attribs.iter_mut().find(|a| a.tag.eq_ignore_ascii_case(attribute))
        {
            a.text.value = tag.to_string();
        }
    })?;
    Ok(())
}

/// The `reserved` parameter: tags or numbers already used elsewhere (at most [`MAX_DEVICES`]).
fn reserved_param(p: &Value) -> HashSet<String> {
    p.get("reserved")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).take(MAX_DEVICES).map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect())
        .unwrap_or_default()
}

/// What ETAG does to one device.
struct Planned {
    dev: Device,
    tag: String,
}

fn run_etag(s: &mut Session, p: &Value) -> Result<Value> {
    let r = run_etag_inner(s, p)?;
    // Keep the project database in step with the drawing.
    sync::auto(s);
    Ok(r)
}

fn run_etag_inner(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "etag";
    let renumber_all = match str_param(p, "mode").map(str::to_ascii_lowercase).as_deref() {
        None | Some("new") => false,
        Some("all") => true,
        Some(m) => return Err(bad(CMD, format!("`mode` must be \"new\" or \"all\", not \"{m}\""))),
    };
    let attribute = attribute_param(p);
    let format = str_param(p, "format").unwrap_or(FORMAT);
    if format.chars().count() > MAX_FORMAT || !format.contains("{class}") {
        return Err(bad(CMD, format!("`format` must contain {{class}} and be at most {MAX_FORMAT} characters")));
    }
    let counted = format.contains("{count}");
    let grid = Grid::from_params(CMD, p)?;
    let overrides = class_overrides(CMD, p)?;
    let prefix = aspects(p);
    if prefix.chars().count() > MAX_FORMAT {
        return Err(bad(CMD, "`function` and `location` are too long"));
    }
    let dry_run = bool_or(p, "dryRun", false);
    let handles = handle_list(CMD, p)?;

    // Every device in the space; those outside `handles` keep their tags and reserve them.
    // Auxiliary parts (contacts) carry their main device's tag: never tagged on their own, they
    // follow a renamed main device.
    let (aux, all): (Vec<Device>, Vec<Device>) = devices(s, attribute.as_deref(), None)?.into_iter().partition(|d| is_child(d, &overrides));
    let mut main_tags: HashMap<String, usize> = HashMap::new();
    for d in all.iter().filter(|d| is_numbered(&d.tag)) {
        *main_tags.entry(d.tag.clone()).or_insert(0) += 1;
    }
    let targeted: Option<HashSet<Handle>> = handles.map(|h| h.into_iter().collect());
    let in_scope = |d: &Device| targeted.as_ref().is_none_or(|t| t.contains(&d.handle));

    // Tags taken elsewhere (other drawings of a project).
    let mut used: HashSet<String> = reserved_param(p);
    let mut todo: Vec<(Device, String, u64, u64)> = Vec::new();
    let (mut kept, mut unclassified, mut outside) = (0usize, Vec::new(), Vec::new());
    for dev in all {
        // Terminals carry their strip's tag (-X1), numbered by ETERMNUM.
        if dev.family.as_deref() == Some("TERMINAL") {
            continue;
        }
        let keep = !in_scope(&dev) || (!renumber_all && is_numbered(&dev.tag));
        if keep {
            if !dev.tag.is_empty() {
                used.insert(dev.tag.clone());
            }
            kept += usize::from(in_scope(&dev));
            continue;
        }
        let Some(class) = class_of(&dev, &overrides) else {
            unclassified.push(dev.handle.hex());
            continue;
        };
        let Some((sheet, column)) = grid.locate(dev.x) else {
            outside.push(dev.handle.hex());
            continue;
        };
        todo.push((dev, class, sheet, column));
    }
    // Reading order: sheet, column, then top to bottom and left to right.
    todo.sort_by(|a, b| (a.2, a.3).cmp(&(b.2, b.3)).then(b.0.y.total_cmp(&a.0.y)).then(a.0.x.total_cmp(&b.0.x)));

    let mut counters: BTreeMap<String, u64> = BTreeMap::new();
    let mut planned = Vec::with_capacity(todo.len());
    for (dev, class, sheet, column) in todo {
        let tag = if counted {
            let key = render(format, &class, sheet, column, None);
            let n = counters.entry(key).or_insert(0);
            let mut found = None;
            for _ in 0..MAX_DEVICES {
                *n = n.saturating_add(1);
                let t = format!("{prefix}{}", render(format, &class, sheet, column, Some(*n)));
                if !used.contains(&t) {
                    found = Some(t);
                    break;
                }
            }
            found
        } else {
            unique(&format!("{prefix}{}", render(format, &class, sheet, column, None)), &used)
        };
        let Some(tag) = tag.filter(|t| t.len() <= MAX_TAG) else {
            return Err(EngineError::Other(format!("Could not make a unique tag for {}.", dev.handle.hex())));
        };
        used.insert(tag.clone());
        planned.push(Planned { dev, tag });
    }

    let (mut tagged, mut followed) = (0usize, 0usize);
    if !dry_run {
        for pl in &planned {
            if pl.tag == pl.dev.tag {
                continue;
            }
            let mut changed = false;
            s.doc_mut()?.modify_entity(pl.dev.handle, |e| {
                if let EntityKind::Insert(ins) = &mut e.kind
                    && let Some(a) = ins.attribs.iter_mut().find(|a| a.tag.eq_ignore_ascii_case(&pl.dev.attr))
                {
                    a.text.value = pl.tag.clone();
                    changed = true;
                }
            })?;
            tagged += usize::from(changed);
        }
        for pl in planned.iter().filter(|pl| pl.tag != pl.dev.tag && is_numbered(&pl.dev.tag) && main_tags.get(&pl.dev.tag) == Some(&1)) {
            for a in aux.iter().filter(|a| a.tag == pl.dev.tag) {
                set_tag(s, a.handle, &a.attr, &pl.tag)?;
                followed += 1;
            }
        }
        s.log.push(format!(
            "{tagged} device(s) tagged, {kept} kept{}{}.",
            if unclassified.is_empty() { String::new() } else { format!(", {} without a class", unclassified.len()) },
            if outside.is_empty() { String::new() } else { format!(", {} outside the sheets", outside.len()) },
        ));
    }
    let list: Vec<Value> =
        planned.iter().map(|pl| json!({ "handle": pl.dev.handle.hex(), "block": pl.dev.block, "old": pl.dev.tag, "tag": pl.tag })).collect();
    Ok(
        json!({ "tagged": tagged, "kept": kept, "contactsFollowed": followed, "auxiliary": aux.len(), "devices": list, "unclassified": unclassified, "outside": outside, "dryRun": dry_run }),
    )
}

fn run_edevices(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "edevices";
    let attribute = attribute_param(p);
    let grid = Grid::from_params(CMD, p)?;
    let overrides = class_overrides(CMD, p)?;
    let devs = devices(s, attribute.as_deref(), None)?;
    let mut by_tag: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut untagged = 0usize;
    let list: Vec<Value> = devs
        .iter()
        .map(|d| {
            // Contacts share their coil's tag and terminals their strip's: neither is a duplicate.
            let auxiliary = is_child(d, &overrides) || d.family.as_deref() == Some("TERMINAL");
            // A contact shares its coil's tag: neither a duplicate nor untagged.
            if !auxiliary && is_numbered(&d.tag) {
                by_tag.entry(d.tag.clone()).or_default().push(d.handle.hex());
            } else if !auxiliary {
                untagged += 1;
            }
            let at = grid.locate(d.x);
            json!({
                "handle": d.handle.hex(),
                "block": d.block,
                "tag": d.tag,
                "class": class_of(d, &overrides),
                "auxiliary": auxiliary,
                "sheet": at.map(|a| a.0),
                "column": at.map(|a| a.1),
            })
        })
        .collect();
    let duplicates: Map<String, Value> = by_tag.into_iter().filter(|(_, hs)| hs.len() > 1).map(|(t, hs)| (t, json!(hs))).collect();
    let msg = format!("{} device(s), {} untagged, {} duplicate tag(s).", list.len(), untagged, duplicates.len());
    s.log.push(msg);
    Ok(json!({ "devices": list, "duplicates": duplicates, "untagged": untagged }))
}

/// ETAG at the command line: `Tag devices [New/All] <New>:`.
struct EtagM;

impl Interactive for EtagM {
    fn name(&self) -> &'static str {
        "ETAG"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Tag devices", KW).kw(&["New", "All"]).default("New")
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let mode = match i {
            Input::Enter => "new",
            Input::Keyword(k) | Input::Text(k) => match k.trim().to_ascii_lowercase().as_str() {
                "new" | "n" | "" => "new",
                "all" | "a" => "all",
                _ => return Err(EngineError::Other("Invalid option keyword.".into())),
            },
            _ => return Ok(Step::Continue),
        };
        run_etag(s, &json!({ "mode": mode }))?;
        Ok(Step::Done)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A drawing with blocks RELAY, MOTOR and LAMP, each with a TAG attribute defaulting to ``.
    fn session() -> Session {
        let mut s = Session::new();
        for name in ["RELAY", "MOTOR", "PUSHBUTTON", "WIDGET"] {
            s.execute("attdef", &json!({"tag": "TAG", "at": [0, 3], "default": "", "height": 2.5})).unwrap();
            let def = s.doc().unwrap().model.last().unwrap().handle;
            s.execute("block", &json!({"name": name, "base": [0, 0], "handles": [def.hex()], "keep": "delete"})).unwrap();
        }
        s
    }

    fn put(s: &mut Session, block: &str, at: [f64; 2], tag: &str) -> String {
        s.execute("insert", &json!({"name": block, "at": at, "attribs": {"TAG": tag}})).unwrap();
        // The newest block reference (live cross-referencing may add texts after it).
        s.doc().unwrap().model.iter().filter(|e| matches!(e.kind, EntityKind::Insert(_))).last().unwrap().handle.hex()
    }

    fn tag_of(s: &Session, h: &str) -> String {
        let h = Handle::parse_hex(h).unwrap();
        match &s.doc().unwrap().entity(h).unwrap().kind {
            EntityKind::Insert(i) => i.attribs[0].text.value.clone(),
            _ => panic!("not an insert"),
        }
    }

    #[test]
    fn sheet_and_column_tags() {
        let mut s = session();
        let k1 = put(&mut s, "RELAY", [100.0, 50.0], "");
        let k2 = put(&mut s, "RELAY", [110.0, 20.0], ""); // same column (3), lower
        let m = put(&mut s, "MOTOR", [430.0, 0.0], ""); // sheet 2, column 1
        let sb = put(&mut s, "PUSHBUTTON", [5.0, 0.0], "");
        let r = s.execute("etag", &json!({})).unwrap();
        assert_eq!(r["tagged"], 4);
        assert_eq!(tag_of(&s, &k1), "-K103");
        assert_eq!(tag_of(&s, &k2), "-K103A");
        assert_eq!(tag_of(&s, &m), "-M201");
        assert_eq!(tag_of(&s, &sb), "-S101");
    }

    #[test]
    fn new_mode_keeps_numbered_tags_and_avoids_them() {
        let mut s = session();
        let kept = put(&mut s, "RELAY", [300.0, 0.0], "-K103");
        let fresh = put(&mut s, "RELAY", [100.0, 0.0], "-K?");
        let r = s.execute("etag", &json!({})).unwrap();
        assert_eq!(r["kept"], 1);
        assert_eq!(tag_of(&s, &kept), "-K103");
        assert_eq!(tag_of(&s, &fresh), "-K103A", "no duplicate of a kept tag");
        // Renumbering everything sorts by place again.
        s.execute("etag", &json!({"mode": "all"})).unwrap();
        assert_eq!(tag_of(&s, &fresh), "-K103");
        assert_eq!(tag_of(&s, &kept), "-K108");
    }

    #[test]
    fn sequential_format_aspects_and_overrides() {
        let mut s = session();
        let a = put(&mut s, "RELAY", [10.0, 0.0], "");
        let b = put(&mut s, "RELAY", [500.0, 0.0], "");
        let w = put(&mut s, "WIDGET", [20.0, 0.0], "");
        let r = s.execute("etag", &json!({"format": "-{class}{count}", "function": "A1", "location": "+M2"})).unwrap();
        assert_eq!(tag_of(&s, &a), "=A1+M2-K1");
        assert_eq!(tag_of(&s, &b), "=A1+M2-K2");
        assert_eq!(r["unclassified"], json!([w]));
        s.execute("etag", &json!({"classes": {"widget": "KF"}})).unwrap();
        assert_eq!(tag_of(&s, &w), "-KF101");
    }

    #[test]
    fn class_from_existing_tag_wins_over_block_name() {
        let mut s = session();
        let h = put(&mut s, "RELAY", [10.0, 0.0], "-Q");
        s.execute("etag", &json!({})).unwrap();
        assert_eq!(tag_of(&s, &h), "-Q101");
    }

    #[test]
    fn dry_run_selection_and_outside() {
        let mut s = session();
        let a = put(&mut s, "RELAY", [10.0, 0.0], "");
        let b = put(&mut s, "RELAY", [-10.0, 0.0], "");
        let c = put(&mut s, "MOTOR", [10.0, 0.0], "");
        let r = s.execute("etag", &json!({"dryRun": true})).unwrap();
        assert_eq!(r["tagged"], 0);
        assert_eq!(r["outside"], json!([b]));
        assert_eq!(tag_of(&s, &a), "");
        s.execute("etag", &json!({"handles": [c]})).unwrap();
        assert_eq!(tag_of(&s, &c), "-M101");
        assert_eq!(tag_of(&s, &a), "");
    }

    #[test]
    fn list_reports_duplicates() {
        let mut s = session();
        let a = put(&mut s, "RELAY", [10.0, 0.0], "-K1.1");
        let b = put(&mut s, "RELAY", [20.0, 0.0], "-K1.1");
        put(&mut s, "MOTOR", [20.0, 0.0], "");
        let r = s.execute("edevices", &json!({})).unwrap();
        assert_eq!(r["devices"].as_array().unwrap().len(), 3);
        assert_eq!(r["untagged"], 1);
        assert_eq!(r["duplicates"]["-K1.1"], json!([a, b]));
    }

    #[test]
    fn undo_restores_tags() {
        let mut s = session();
        let a = put(&mut s, "RELAY", [10.0, 0.0], "");
        s.execute("etag", &json!({})).unwrap();
        assert_eq!(tag_of(&s, &a), "-K101");
        s.execute("undo", &json!({})).unwrap();
        assert_eq!(tag_of(&s, &a), "");
    }

    #[test]
    fn command_line_form() {
        let mut s = session();
        let a = put(&mut s, "RELAY", [10.0, 0.0], "");
        s.cmdline("etag").unwrap();
        s.cmdline("all").unwrap();
        assert!(s.running.is_none());
        assert_eq!(tag_of(&s, &a), "-K101");
    }

    #[test]
    fn copies_of_a_device_get_their_own_tag() {
        let mut s = Session::new();
        let k = s.execute("esymbol", &json!({ "name": "IEC_COIL", "at": [21, 100] })).unwrap();
        assert_eq!(k["tag"], "-K101");
        let c = s.execute("esymbol", &json!({ "name": "IEC_NO", "at": [63, 150], "tag": "-K101" })).unwrap();
        // Copy both two columns right: the coil's copy is retagged, the contact's copy keeps the
        // tag it carries (a second contact of the same coil).
        s.execute("copy", &json!({ "handles": [k["handle"], c["handle"]], "delta": [84, 0] })).unwrap();
        let tags: Vec<(String, String)> = s
            .doc()
            .unwrap()
            .model
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Insert(i) => {
                    i.attribs.iter().find(|a| a.tag == "TAG1" || a.tag == "TAG2").map(|a| (i.block.clone(), a.text.value.clone()))
                }
                _ => None,
            })
            .collect();
        let coils: Vec<&str> = tags.iter().filter(|t| t.0 == "IEC_COIL").map(|t| t.1.as_str()).collect();
        assert_eq!(coils.len(), 2, "{tags:?}");
        assert_eq!(coils[0], "-K101");
        assert!(coils[1].starts_with("-K") && coils[1] != "-K101", "{tags:?}");
        let contacts: Vec<&str> = tags.iter().filter(|t| t.0 == "IEC_NO").map(|t| t.1.as_str()).collect();
        assert_eq!(contacts, vec!["-K101", "-K101"]);
    }

    #[test]
    fn hostile_params_are_refused() {
        let mut s = session();
        put(&mut s, "RELAY", [1e308, -1e308], "");
        for p in [
            json!({"sheetWidth": 0}),
            json!({"sheetWidth": -5}),
            json!({"columns": 0}),
            json!({"columns": 1e12}),
            json!({"format": "{sheet}"}),
            json!({"format": "x".repeat(100) + "{class}"}),
            json!({"classes": {"RELAY": "zz"}}),
            json!({"classes": 5}),
            json!({"mode": "sideways"}),
            json!({"handles": "1"}),
            json!({"firstSheet": -1}),
        ] {
            assert!(s.execute("etag", &p).is_err(), "{p}");
        }
        // Far-off devices are reported, not tagged.
        let r = s.execute("etag", &json!({"firstSheet": 99999})).unwrap();
        assert_eq!(r["outside"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn grid_locates_columns() {
        let g = Grid { origin_x: 0.0, sheet_width: 420.0, columns: 10, first_sheet: 1 };
        assert_eq!(g.locate(0.0), Some((1, 1)));
        assert_eq!(g.locate(41.9), Some((1, 1)));
        assert_eq!(g.locate(42.0), Some((1, 2)));
        assert_eq!(g.locate(419.999), Some((1, 10)));
        assert_eq!(g.locate(420.0), Some((2, 1)));
        assert_eq!(g.locate(-0.1), None);
        assert_eq!(g.locate(f64::NAN), None);
        assert_eq!(g.locate(f64::INFINITY), None);
    }
}
