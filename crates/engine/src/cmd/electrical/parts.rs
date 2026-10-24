//! Manufacturer parts: PLC modules and drives drawn from their real terminal lists, like the
//! macros of the EPLAN Data Portal or AutoCAD Electrical's PLC database.
//!
//! EPART places a part from the catalogue (or one described in the call, `spec`) as one block:
//! a box with every terminal of the part on its top (inputs, supply, mains) or bottom edge
//! (outputs, motor), each with its terminal number inside the box, its function beside the lead
//! and, for I/O points, its PLC address in a small frame. The manufacturer, catalogue number,
//! rack/slot lines, description and tag stand in a column on the box's left, as in AutoCAD
//! Electrical. A part can also be placed one group at a time (a drive's power and control
//! terminals on different pages): the first group carries the tag (`TAG1`), the others follow it
//! as child parts (`TAG2`) and are cross-referenced to it.
//!
//! Addresses come from the part's address formats: `{rack}I{slot}.{n2}` (Allen-Bradley in
//! AutoCAD Electrical style, `A2:I20.02`), `%I{byte}.{bit}` (Siemens), `X{o}` (Mitsubishi, octal).
//!
//! The terminal data come from the manufacturers' published manuals and wiring diagrams; parts
//! not yet checked against a real drawing say so (`checked: false`). The drawings are our own.

use std::f64::consts::FRAC_PI_2;
use std::sync::{Arc, OnceLock};

use cadcraft_doc::{Block, Circle, Common, Entity, EntityKind, HAlign, Line, VAlign};
use cadcraft_geom::{Vec2, Vec3};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::symbols::attdef;
use crate::cmd::{CommandSpec, bad, f64_or, point_req, str_param};
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

const CMD: &str = "epart";
/// Height of a part's box.
const BOX_H: f64 = 18.0;
/// Length of a terminal's lead outside the box.
const LEAD: f64 = 5.0;
/// Most terminals on one part (the `TERMnn` attributes have two digits).
const MAX_TERMS: usize = 99;
const MAX_GROUPS: usize = 16;
/// Longest text accepted in a part described by the caller.
const MAX_TEXT: usize = 64;

/// What an I/O point is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub(super) enum Io {
    Di,
    Do,
    Ai,
    Ao,
}

impl Io {
    const ALL: [Io; 4] = [Io::Di, Io::Do, Io::Ai, Io::Ao];
    fn index(self) -> usize {
        match self {
            Io::Di => 0,
            Io::Do => 1,
            Io::Ai => 2,
            Io::Ao => 3,
        }
    }
    fn name(self) -> &'static str {
        ["DI", "DO", "AI", "AO"][self.index().min(3)]
    }
    fn parse(t: &str) -> Option<Io> {
        match t.trim().to_ascii_uppercase().as_str() {
            "DI" | "I" | "IN" => Some(Io::Di),
            "DO" | "DQ" | "Q" | "O" | "OUT" => Some(Io::Do),
            "AI" => Some(Io::Ai),
            "AO" | "AQ" => Some(Io::Ao),
            _ => None,
        }
    }
}

/// One terminal: its number on the device, what it does, whether it is an I/O point. An empty
/// number is a gap (an empty column).
#[derive(Clone, Debug, Deserialize)]
pub(super) struct Term {
    #[serde(default)]
    pub num: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub io: Option<Io>,
}

/// Terminals placed together: those on the box's top edge and those on its bottom edge.
#[derive(Clone, Debug, Deserialize)]
pub(super) struct Group {
    pub name: String,
    #[serde(default)]
    pub top: Vec<Term>,
    #[serde(default)]
    pub bottom: Vec<Term>,
}

/// A catalogue part.
#[derive(Clone, Debug)]
pub(super) struct Part {
    pub mfg: String,
    pub cat: String,
    /// `PLC` or `VFD`.
    pub family: String,
    /// IEC 81346-2 class letters (`KF` for a PLC module, `TA` for a drive).
    pub class: String,
    pub series: String,
    pub description: String,
    /// A drive's rating (`4 kW, 10.5 A`).
    pub rating: String,
    /// Address formats per kind (DI, DO, AI, AO), and the default first number of each.
    pub formats: [String; 4],
    pub starts: [String; 4],
    pub groups: Vec<Group>,
    /// Checked against a drawing made with the real part.
    pub checked: bool,
}

impl Part {
    /// Every terminal, group by group, top then bottom, gaps left out.
    fn terms(&self) -> impl Iterator<Item = &Term> {
        self.groups.iter().flat_map(|g| g.top.iter().chain(g.bottom.iter())).filter(|t| !t.num.is_empty())
    }
    fn io_counts(&self) -> [usize; 4] {
        let mut n = [0usize; 4];
        for t in self.terms() {
            if let Some(io) = t.io
                && let Some(c) = n.get_mut(io.index())
            {
                *c += 1;
            }
        }
        n
    }
    fn summary(&self) -> Value {
        let n = self.io_counts();
        json!({
            "mfg": self.mfg, "cat": self.cat, "family": self.family, "class": self.class, "series": self.series,
            "description": self.description, "rating": self.rating, "checked": self.checked,
            "groups": self.groups.iter().map(|g| g.name.clone()).collect::<Vec<_>>(),
            "io": { "DI": n[0], "DO": n[1], "AI": n[2], "AO": n[3] },
            "terminals": self.terms().count(),
        })
    }
}

// ----------------------------------------------------------------------------------------------
// Catalogue.

fn t(num: &str, label: &str) -> Term {
    Term { num: num.into(), label: label.into(), io: None }
}
fn io(num: &str, label: &str, kind: Io) -> Term {
    Term { num: num.into(), label: label.into(), io: Some(kind) }
}
fn gap() -> Term {
    Term { num: String::new(), label: String::new(), io: None }
}
/// Points `from..to` numbered `num(i)`, labelled `label(i)`.
fn points(range: std::ops::Range<u32>, kind: Io, num: impl Fn(u32) -> String, label: impl Fn(u32) -> String) -> Vec<Term> {
    range.map(|i| Term { num: num(i), label: label(i), io: Some(kind) }).collect()
}
fn group(name: &str, top: Vec<Term>, bottom: Vec<Term>) -> Group {
    Group { name: name.into(), top, bottom }
}

#[derive(Clone, Copy)]
struct PartBase<'a> {
    mfg: &'a str,
    family: &'a str,
    class: &'a str,
    series: &'a str,
    formats: [&'a str; 4],
    starts: [&'a str; 4],
}

impl PartBase<'_> {
    fn part(&self, cat: &str, description: &str, rating: &str, groups: Vec<Group>, checked: bool) -> Part {
        Part {
            mfg: self.mfg.into(),
            cat: cat.into(),
            family: self.family.into(),
            class: self.class.into(),
            series: self.series.into(),
            description: description.into(),
            rating: rating.into(),
            formats: self.formats.map(String::from),
            starts: self.starts.map(String::from),
            groups,
            checked,
        }
    }
    fn starts(mut self, s: [&'static str; 4]) -> Self {
        self.starts = s;
        self
    }
}

/// Allen-Bradley I/O, addresses in AutoCAD Electrical style (`I1.00`, `A2:O32.07`).
const AB_FORMATS: [&str; 4] = ["{rack}I{slot}.{n2}", "{rack}O{slot}.{n2}", "{rack}AI{slot}.{n2}", "{rack}AO{slot}.{n2}"];
const SIEMENS_FORMATS: [&str; 4] = ["%I{byte}.{bit}", "%Q{byte}.{bit}", "%IW{word}", "%QW{word}"];
const MITSUBISHI_FORMATS: [&str; 4] = ["X{o}", "Y{o}", "D{n}", "D{n}"];
const NO_FORMATS: [&str; 4] = ["", "", "", ""];
const ZERO: [&str; 4] = ["0", "0", "0", "0"];

fn catalogue() -> Vec<Part> {
    let mut v = Vec::new();
    v.extend(allen_bradley());
    v.extend(powerflex());
    v.extend(siemens());
    v.extend(mitsubishi());
    v
}

fn allen_bradley() -> Vec<Part> {
    let c5k = PartBase { mfg: "ALLEN BRADLEY", family: "PLC", class: "KF", series: "5069 LOGIX", formats: AB_FORMATS, starts: ZERO };
    let pt = PartBase { series: "1734 POINT I/O", ..c5k };
    let n = |i: u32| i.to_string();
    let mut ib16 = points(0..16, Io::Di, n, |i| format!("IN {i}"));
    ib16.extend([t("16", "NC"), t("17", "NC")]);
    let mut ob16 = points(0..16, Io::Do, n, |i| format!("OUT {i}"));
    ob16.extend([t("16", "LA+"), t("17", "LA-")]);
    let mut ob8s = Vec::new();
    for i in 0..8u32 {
        ob8s.push(io(&(2 * i).to_string(), &format!("SF (P) OUT {i}"), Io::Do));
        ob8s.push(t(&(2 * i + 1).to_string(), &format!("SF (N) OUT {i}")));
    }
    ob8s.extend([t("16", "LA+"), t("17", "LA-")]);
    let ib8s_top = points(0..8, Io::Di, n, |i| format!("SF IN {i}"));
    let ib8s_bottom = vec![
        t("8", "TST OP 0"),
        t("9", "TST OP 0"),
        t("10", "TST OP 1"),
        t("11", "TST OP 1"),
        t("12", "TST/MUTE OP 2"),
        t("13", "TST/MUTE OP 2"),
        t("14", "TST/MUTE OP 3"),
        t("15", "TST/MUTE OP 3"),
        gap(),
        t("16", "COM"),
        t("17", "COM"),
    ];
    let cpu = vec![
        t("PORT1", "PORT1"),
        t("PORT2", "PORT2"),
        gap(),
        t("MP+", "MOD POWER +"),
        t("MP+", "MOD POWER +"),
        t("MP-", "MOD POWER -"),
        t("MP-", "MOD POWER -"),
        gap(),
        t("SA+", "SA POWER +"),
        t("SA+", "SA POWER +"),
        t("SA-", "SA POWER -"),
        t("SA-", "SA POWER -"),
        t("GND", "SA POWER GND"),
        t("GND", "SA POWER GND"),
    ];
    vec![
        c5k.part("5069-L320ERS2", "COMPACT GUARDLOGIX 5380 L320 CPU", "", vec![group("power", cpu.clone(), vec![])], true),
        c5k.part("5069-L320ER", "COMPACTLOGIX 5380 L320 CPU", "", vec![group("power", cpu.clone(), vec![])], false),
        c5k.part("5069-L330ER", "COMPACTLOGIX 5380 L330 CPU", "", vec![group("power", cpu, vec![])], false),
        c5k.part("5069-IB16", "24VDC 16-PT SINKING INPUT", "", vec![group("inputs", ib16, vec![])], true),
        c5k.part("5069-OB16", "24VDC 16-PT SOURCE OUTPUT", "", vec![group("outputs", vec![], ob16)], true),
        c5k.part("5069-IB8S", "24VDC 8-PT SAFETY INPUT", "", vec![group("inputs", ib8s_top, ib8s_bottom)], true),
        c5k.part("5069-OBV8S", "SAFE 8-PT OUTPUT (BIPOLAR)", "", vec![group("outputs", vec![], ob8s)], true),
        pt.part("1734-IB8", "8-PT 24VDC DIGITAL INPUT", "", vec![group("inputs", points(0..8, Io::Di, n, |i| format!("IN {i}")), vec![])], true),
        pt.part("1734-OB8", "8-PT 24VDC DIGITAL OUTPUT", "", vec![group("outputs", vec![], points(0..8, Io::Do, n, |i| format!("OUT {i}")))], true),
        pt.part(
            "1734-VTM",
            "I/O VOLTAGE TERMINAL MODULE",
            "",
            vec![group("terminals", (0..8).map(|i| t(&i.to_string(), "V")).collect(), vec![])],
            true,
        ),
        pt.part(
            "1734-CTM",
            "I/O COMMON TERMINAL MODULE",
            "",
            vec![group("terminals", (0..8).map(|i| t(&i.to_string(), "C")).collect(), vec![])],
            true,
        ),
    ]
}

fn powerflex() -> Vec<Part> {
    let pf = PartBase { mfg: "ALLEN BRADLEY", family: "VFD", class: "TA", series: "POWERFLEX 525", formats: NO_FORMATS, starts: ZERO };
    let power = || {
        group(
            "power",
            vec![t("L1", "R/L1"), t("L2", "S/L2"), t("L3", "T/L3"), t("PE", "PE")],
            vec![t("U", "U/T1"), t("V", "V/T2"), t("W", "W/T3"), t("PE", "PE"), gap(), t("BR+", "BRAKE R"), t("BR-", "BRAKE R")],
        )
    };
    let control = || {
        group(
            "control",
            vec![
                t("11", "+24VDC"),
                t("04", "DI COM"),
                t("01", "STOP"),
                t("02", "FWD"),
                t("03", "REV"),
                t("05", "DI 1"),
                t("06", "DI 2"),
                t("07", "DI 3"),
                t("08", "DI 4"),
                t("12", "+10VDC"),
                t("13", "0-10V SIG"),
                t("14", "ANG COM"),
                t("15", "4-20mA SIG"),
                t("S1", "STO 1"),
                t("S2", "STO 2"),
                t("S+", "STO +24V"),
                t("PORT1", "ETHERNET"),
            ],
            vec![
                t("16", "ANG OUT"),
                t("17", "OPTO OP1"),
                t("18", "OPTO OP2"),
                t("19", "OPTO COM"),
                gap(),
                t("R1", "RLY1 NO"),
                t("R2", "RLY1 COM"),
                t("R5", "RLY2 COM"),
                t("R6", "RLY2 NC"),
            ],
        )
    };
    // 400 V three-phase frames: catalogue number, rating.
    [
        ("25B-D1P4N114", "0.4KW, 1.4A"),
        ("25B-D2P3N114", "0.75KW, 2.3A"),
        ("25B-D4P0N114", "1.5KW, 4.0A"),
        ("25B-D6P0N114", "2.2KW, 6.0A"),
        ("25B-D010N114", "4KW, 10.5A"),
        ("25B-D013N114", "5.5KW, 13A"),
        ("25B-D017N114", "7.5KW, 17A"),
        ("25B-D024N114", "11KW, 24A"),
        ("25B-D030N114", "15KW, 30A"),
        ("25B-D037N114", "18.5KW, 37A"),
        ("25B-D043N114", "22KW, 43A"),
    ]
    .into_iter()
    .map(|(cat, rating)| pf.part(cat, "POWERFLEX 525 AC DRIVE 400V", rating, vec![power(), control()], true))
    .collect()
}

fn siemens() -> Vec<Part> {
    let s7 = PartBase { mfg: "SIEMENS", family: "PLC", class: "KF", series: "S7-1200", formats: SIEMENS_FORMATS, starts: ["0", "0", "64", "64"] };
    let cpu_dc = group(
        "DI/DQ",
        {
            let mut v = vec![
                t("L+", "24VDC IN"),
                t("M", "0V IN"),
                t("FE", "FUNC EARTH"),
                t("L+", "24V SENSOR OUT"),
                t("M", "0V SENSOR OUT"),
                t("1M", "DI COM"),
            ];
            v.extend(points(0..8, Io::Di, |i| format!("DIa.{i}"), |i| format!("DI a.{i}")));
            v.extend(points(0..6, Io::Di, |i| format!("DIb.{i}"), |i| format!("DI b.{i}")));
            v
        },
        {
            let mut v = vec![t("3L+", "DQ 24VDC"), t("3M", "DQ 0V")];
            v.extend(points(0..8, Io::Do, |i| format!("DQa.{i}"), |i| format!("DQ a.{i}")));
            v.extend(points(0..2, Io::Do, |i| format!("DQb.{i}"), |i| format!("DQ b.{i}")));
            v
        },
    );
    let cpu_relay = group(
        "DI/DQ",
        {
            let mut v = vec![
                t("L1", "120/230VAC L"),
                t("N", "N"),
                t("FE", "FUNC EARTH"),
                t("L+", "24V SENSOR OUT"),
                t("M", "0V SENSOR OUT"),
                t("1M", "DI COM"),
            ];
            v.extend(points(0..8, Io::Di, |i| format!("DIa.{i}"), |i| format!("DI a.{i}")));
            v.extend(points(0..6, Io::Di, |i| format!("DIb.{i}"), |i| format!("DI b.{i}")));
            v
        },
        {
            let mut v = vec![t("1L", "RELAY COM 1")];
            v.extend(points(0..5, Io::Do, |i| format!("DQa.{i}"), |i| format!("DQ a.{i}")));
            v.push(t("2L", "RELAY COM 2"));
            v.extend(points(5..8, Io::Do, |i| format!("DQa.{i}"), |i| format!("DQ a.{i}")));
            v.extend(points(0..2, Io::Do, |i| format!("DQb.{i}"), |i| format!("DQ b.{i}")));
            v
        },
    );
    let cpu_ai = || group("AI", vec![t("2M", "AI COM"), io("AI0", "AI 0", Io::Ai), io("AI1", "AI 1", Io::Ai)], vec![]);
    let sm1221 = group(
        "DI",
        {
            let mut v = vec![t("1M", "DI COM a")];
            v.extend(points(0..8, Io::Di, |i| format!("DIa.{i}"), |i| format!("DI a.{i}")));
            v.push(t("2M", "DI COM b"));
            v.extend(points(0..8, Io::Di, |i| format!("DIb.{i}"), |i| format!("DI b.{i}")));
            v
        },
        vec![],
    );
    let sm1231 = group(
        "AI",
        {
            let mut v = vec![t("L+", "24VDC"), t("M", "0V")];
            for i in 0..4u32 {
                v.push(io(&format!("{i}+"), &format!("AI {i}+"), Io::Ai));
                v.push(t(&format!("{i}-"), &format!("AI {i}-")));
            }
            v
        },
        vec![],
    );
    let g120 = PartBase { mfg: "SIEMENS", family: "VFD", class: "TA", series: "SINAMICS G120C", formats: NO_FORMATS, starts: ZERO };
    let g120_power = group(
        "power",
        vec![t("L1", "L1"), t("L2", "L2"), t("L3", "L3"), t("PE", "PE")],
        vec![t("U2", "U"), t("V2", "V"), t("W2", "W"), t("PE", "PE"), gap(), t("R1", "BRAKE R"), t("R2", "BRAKE R")],
    );
    let g120_control = group(
        "control",
        vec![
            t("31", "+24V IN"),
            t("32", "GND IN"),
            t("9", "+24V OUT"),
            t("28", "GND"),
            t("69", "DI COM1"),
            t("5", "DI 0"),
            t("6", "DI 1"),
            t("7", "DI 2"),
            t("8", "DI 3"),
            t("34", "DI COM2"),
            t("16", "DI 4 / F-DI"),
            t("17", "DI 5 / F-DI"),
            t("1", "+10V OUT"),
            t("3", "AI 0+"),
            t("4", "AI 0-"),
            t("2", "GND"),
            t("14", "T1 MOTOR"),
            t("15", "T2 MOTOR"),
        ],
        vec![
            t("18", "DO 0 NC"),
            t("19", "DO 0 NO"),
            t("20", "DO 0 COM"),
            gap(),
            t("21", "DO 1+"),
            t("22", "DO 1-"),
            gap(),
            t("12", "AO 0+"),
            t("13", "AO 0-"),
        ],
    );
    vec![
        s7.part("6ES7214-1AG40-0XB0", "CPU 1214C DC/DC/DC", "", vec![cpu_dc, cpu_ai()], false),
        s7.part("6ES7214-1BG40-0XB0", "CPU 1214C AC/DC/RLY", "", vec![cpu_relay, cpu_ai()], false),
        s7.starts(["8", "8", "64", "64"]).part("6ES7221-1BH32-0XB0", "SM 1221 DI 16x24VDC", "", vec![sm1221], false),
        s7.starts(["0", "0", "96", "96"]).part("6ES7231-4HD32-0XB0", "SM 1231 AI 4x13BIT", "", vec![sm1231], false),
        g120.part("6SL3210-1KE...", "SINAMICS G120C INVERTER 400V", "", vec![g120_power, g120_control], false),
    ]
}

fn mitsubishi() -> Vec<Part> {
    let fx = PartBase { mfg: "MITSUBISHI", family: "PLC", class: "KF", series: "MELSEC iQ-F", formats: MITSUBISHI_FORMATS, starts: ZERO };
    let x = |i: u32| format!("X{i:o}");
    let y = |i: u32| format!("Y{i:o}");
    let fx5u32 = group(
        "I/O",
        {
            let mut v = vec![t("L", "100-240VAC L"), t("N", "N"), t("PE", "PE"), t("24V", "24VDC OUT"), t("0V", "0V OUT"), t("S/S", "INPUT COM")];
            v.extend(points(0..16, Io::Di, x, x));
            v
        },
        {
            let mut v = Vec::new();
            for (com, from) in [("COM0", 0u32), ("COM1", 4), ("COM2", 8), ("COM3", 12)] {
                v.push(t(com, com));
                v.extend(points(from..from + 4, Io::Do, y, y));
            }
            v
        },
    );
    let ex16 = group(
        "DI",
        {
            let mut v = vec![t("S/S", "INPUT COM")];
            v.extend(points(0..16, Io::Di, |i| format!("X{i:o}"), |i| format!("X{i:o}")));
            v
        },
        vec![],
    );
    let eyt16 = group("DO", vec![], {
        let mut v = Vec::new();
        for (com, from) in [("COM0", 0u32), ("COM1", 8)] {
            v.push(t(com, com));
            v.extend(points(from..from + 8, Io::Do, |i| format!("Y{i:o}"), |i| format!("Y{i:o}")));
        }
        v
    });
    vec![
        fx.part("FX5U-32MT/ES", "FX5U CPU 16DI/16DO TRANSISTOR (SINK)", "", vec![fx5u32], false),
        fx.starts(["20", "20", "0", "0"]).part("FX5-16EX/ES", "FX5 16-PT 24VDC INPUT", "", vec![ex16], false),
        fx.starts(["20", "20", "0", "0"]).part("FX5-16EYT/ES", "FX5 16-PT TRANSISTOR OUTPUT (SINK)", "", vec![eyt16], false),
    ]
}

pub(super) fn library() -> &'static [Part] {
    static L: OnceLock<Vec<Part>> = OnceLock::new();
    L.get_or_init(catalogue)
}

/// The catalogue part with catalogue number `cat` (any case, spaces ignored).
pub(super) fn find(cat: &str) -> Option<&'static Part> {
    let key = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_uppercase();
    let want = key(cat);
    library().iter().find(|p| key(&p.cat) == want)
}

// ----------------------------------------------------------------------------------------------
// A part described by the caller.

#[derive(Deserialize)]
struct Spec {
    #[serde(default)]
    mfg: String,
    cat: String,
    #[serde(default)]
    family: String,
    #[serde(default)]
    class: String,
    #[serde(default)]
    series: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    rating: String,
    #[serde(default)]
    formats: Map<String, Value>,
    #[serde(default)]
    starts: Map<String, Value>,
    groups: Vec<Group>,
}

fn clip(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(MAX_TEXT).collect()
}

fn part_from_spec(v: &Value) -> Result<Part> {
    let sp: Spec = serde_json::from_value(v.clone()).map_err(|e| bad(CMD, format!("`spec`: {e}")))?;
    if sp.cat.trim().is_empty() {
        return Err(bad(CMD, "`spec.cat` (the catalogue number) is required"));
    }
    if sp.groups.is_empty() || sp.groups.len() > MAX_GROUPS {
        return Err(bad(CMD, format!("`spec.groups`: 1 to {MAX_GROUPS} groups")));
    }
    let family = match sp.family.trim().to_ascii_uppercase().as_str() {
        "" | "PLC" => "PLC",
        "VFD" | "DRIVE" => "VFD",
        _ => return Err(bad(CMD, "`spec.family` must be PLC or VFD")),
    };
    let base = if family == "PLC" { AB_FORMATS } else { NO_FORMATS };
    let mut formats = base.map(String::from);
    let mut starts = ZERO.map(String::from);
    for (k, val) in &sp.formats {
        let io = Io::parse(k).ok_or_else(|| bad(CMD, format!("`spec.formats.{k}`: kinds are DI, DO, AI, AO")))?;
        if let (Some(slot), Some(f)) = (formats.get_mut(io.index()), val.as_str()) {
            *slot = clip(f);
        }
    }
    for (k, val) in &sp.starts {
        let io = Io::parse(k).ok_or_else(|| bad(CMD, format!("`spec.starts.{k}`: kinds are DI, DO, AI, AO")))?;
        if let Some(slot) = starts.get_mut(io.index()) {
            *slot = clip(&value_text(val));
        }
    }
    let groups: Vec<Group> = sp
        .groups
        .iter()
        .map(|g| {
            let fix = |ts: &[Term]| ts.iter().map(|x| Term { num: clip(&x.num), label: clip(&x.label), io: x.io }).collect::<Vec<_>>();
            Group { name: clip(&g.name), top: fix(&g.top), bottom: fix(&g.bottom) }
        })
        .collect();
    let class = clip(sp.class.trim());
    let part = Part {
        mfg: clip(&sp.mfg),
        cat: clip(sp.cat.trim()),
        family: family.into(),
        class: if class.is_empty() { (if family == "PLC" { "KF" } else { "TA" }).into() } else { class.to_ascii_uppercase() },
        series: clip(&sp.series),
        description: clip(&sp.description),
        rating: clip(&sp.rating),
        formats,
        starts,
        groups,
        checked: false,
    };
    let total: usize = part.groups.iter().map(|g| g.top.len() + g.bottom.len()).sum();
    if part.terms().count() == 0 || total > MAX_TERMS * 2 || part.terms().count() > MAX_TERMS {
        return Err(bad(CMD, format!("`spec`: 1 to {MAX_TERMS} terminals")));
    }
    Ok(part)
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

// ----------------------------------------------------------------------------------------------
// Addresses.

/// Address options: the rack prefix (`A2:`), the slot and the first number of each kind.
#[derive(Clone, Debug, Default)]
pub(super) struct AddrOpts {
    pub rack: String,
    pub slot: String,
    pub starts: [Option<String>; 4],
    pub formats: [Option<String>; 4],
}

fn parse_base(s: &str, radix: u32) -> u64 {
    u64::from_str_radix(s.trim(), radix).unwrap_or(0).min(1 << 40)
}

/// The address of the `idx`-th point of a kind: `format` with its placeholders filled.
pub(super) fn address(format: &str, start: &str, idx: u64, rack: &str, slot: &str) -> String {
    if format.is_empty() {
        return String::new();
    }
    let mut out = format.replace("{rack}", rack).replace("{slot}", slot);
    if out.contains("{byte}") || out.contains("{bit}") {
        let total = parse_base(start, 10).saturating_mul(8).saturating_add(idx);
        out = out.replace("{byte}", &(total / 8).to_string()).replace("{bit}", &(total % 8).to_string());
    }
    if out.contains("{word}") {
        out = out.replace("{word}", &parse_base(start, 10).saturating_add(idx.saturating_mul(2)).to_string());
    }
    if out.contains("{o}") {
        out = out.replace("{o}", &format!("{:o}", parse_base(start, 8).saturating_add(idx)));
    }
    if out.contains("{x}") {
        out = out.replace("{x}", &format!("{:X}", parse_base(start, 16).saturating_add(idx)));
    }
    let n = parse_base(start, 10).saturating_add(idx);
    out.replace("{n2}", &format!("{n:02}")).replace("{n}", &n.to_string())
}

/// The address of every terminal of `part` (empty for terminals that are not I/O points), in
/// [`Part::terms`] order.
pub(super) fn addresses(part: &Part, o: &AddrOpts) -> Vec<String> {
    let mut count = [0u64; 4];
    part.terms()
        .map(|term| {
            let Some(kind) = term.io else { return String::new() };
            let k = kind.index();
            let idx = count.get(k).copied().unwrap_or(0);
            if let Some(c) = count.get_mut(k) {
                *c += 1;
            }
            let format = o.formats.get(k).cloned().flatten().unwrap_or_else(|| part.formats.get(k).cloned().unwrap_or_default());
            let start = o.starts.get(k).cloned().flatten().unwrap_or_else(|| part.starts.get(k).cloned().unwrap_or_default());
            address(&format, &start, idx, &o.rack, &o.slot)
        })
        .collect()
}

// ----------------------------------------------------------------------------------------------
// The block.

/// One terminal placed in a block: (index into the part's terminals, column, on top).
struct Placed<'a> {
    term: &'a Term,
    index: usize,
    col: usize,
    top: bool,
}

/// The terminals of the chosen groups and their columns: each group starts in a fresh column,
/// one empty column after the one before; top and bottom of a group start together.
fn layout<'a>(part: &'a Part, groups: &[usize]) -> (Vec<Placed<'a>>, usize) {
    let mut out = Vec::new();
    let mut index = 0usize;
    let mut col = 0usize;
    for (gi, g) in part.groups.iter().enumerate() {
        let chosen = groups.contains(&gi);
        let mut width = 0usize;
        for (side_top, list) in [(true, &g.top), (false, &g.bottom)] {
            for (c, term) in list.iter().enumerate() {
                if term.num.is_empty() {
                    continue;
                }
                if chosen {
                    out.push(Placed { term, index, col: col + c, top: side_top });
                }
                index += 1;
            }
            width = width.max(list.len());
        }
        if chosen && width > 0 {
            col += width + 1;
        }
    }
    (out, col.saturating_sub(1).max(1))
}

/// A text entity.
fn text(at: Vec2, h: f64, value: &str, halign: HAlign, rotated: bool) -> EntityKind {
    let p = Vec3::new(at.x, at.y, 0.0);
    EntityKind::Text(cadcraft_doc::Text {
        insert: p,
        align_pt: Some(p),
        height: h,
        value: value.to_string(),
        rotation: if rotated { FRAC_PI_2 } else { 0.0 },
        width_factor: 1.0,
        oblique: 0.0,
        style: "Standard".into(),
        halign,
        valign: VAlign::Middle,
    })
}

/// An attribute definition turned to read upwards.
fn att(tag: &str, at: Vec2, h: f64, value: &str, invisible: bool, halign: HAlign, rotated: bool) -> EntityKind {
    let mut k = attdef(tag, at, h, value, invisible, halign);
    if rotated && let EntityKind::AttDef(a) = &mut k {
        a.text.rotation = FRAC_PI_2;
    }
    k
}

fn line(a: (f64, f64), b: (f64, f64)) -> EntityKind {
    EntityKind::Line(Line { a: Vec3::new(a.0, a.1, 0.0), b: Vec3::new(b.0, b.1, 0.0) })
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<EntityKind> {
    vec![line((x0, y0), (x1, y0)), line((x1, y0), (x1, y1)), line((x1, y1), (x0, y1)), line((x0, y1), (x0, y0))]
}

/// The pitch between terminals when none is given: wide for a 16-point card, close for long
/// terminal rows.
fn default_pitch(columns: usize) -> f64 {
    match columns {
        0..=18 => 20.0,
        19..=36 => 10.0,
        _ => 7.5,
    }
}

/// The block for `part` (the chosen groups), a child part (`TAG2`) or the main one.
fn build_block(
    part: &Part,
    groups: &[usize],
    child: bool,
    pitch: Option<f64>,
    mut new_handle: impl FnMut() -> cadcraft_doc::Handle,
    name: &str,
) -> Block {
    let (placed, columns) = layout(part, groups);
    let pitch = pitch.unwrap_or_else(|| default_pitch(columns));
    let first = pitch / 2.0;
    let w = pitch * columns as f64;
    let h = BOX_H;
    let mut kinds = rect(0.0, -h, w, 0.0);
    // Columns used on each side, for the address frames.
    let used = |col: usize, top: bool| placed.iter().any(|p| p.col == col && p.top == top);
    for p in &placed {
        let x = first + pitch * p.col as f64;
        let (edge, tip, out) = if p.top { (0.0, LEAD, 1.0) } else { (-h, -h - LEAD, -1.0) };
        kinds.push(EntityKind::Circle(Circle { center: Vec3::new(x, edge, 0.0), radius: 0.6 }));
        kinds.push(line((x, edge + 0.6 * out), (x, tip)));
        // The function beside the lead, outside the box.
        if !p.term.label.is_empty() {
            let at = Vec2::new(x + 1.6, edge + out * 1.2);
            kinds.push(text(at, 1.6, &p.term.label, if p.top { HAlign::Left } else { HAlign::Right }, true));
        }
        let n = p.index + 1;
        // The terminal number inside the box, against its edge.
        let num_at = Vec2::new(x + 1.6, edge - out * 1.2);
        kinds.push(att(&format!("TERM{n:02}"), num_at, 1.6, &p.term.num, false, if p.top { HAlign::Right } else { HAlign::Left }, true));
        // The connection point: left-baseline, so its insertion point is the point itself.
        let dir = if p.top { 1u8 } else { 4u8 };
        let mut cp = attdef(&format!("X{dir}TERM{n:02}"), Vec2::new(x, tip), 1.0, "", true, HAlign::Left);
        if let EntityKind::AttDef(a) = &mut cp {
            a.text.align_pt = None;
            a.text.valign = VAlign::Baseline;
        }
        kinds.push(cp);
        // The address in its frame; the point's own description on the far side of the box.
        if p.term.io.is_some() {
            let both = used(p.col, !p.top);
            let (y0, y1) = match (both, p.top) {
                (false, _) => (-h + 2.5, -2.5),
                (true, true) => (-h / 2.0 + 0.5, -2.5),
                (true, false) => (-h + 2.5, -h / 2.0 - 0.5),
            };
            kinds.extend(rect(x - 1.3, y0, x + 1.3, y1));
            kinds.push(att(&format!("ADDR{n:02}"), Vec2::new(x, (y0 + y1) / 2.0), 1.5, "", false, HAlign::Center, true));
            if !both {
                let (at, al) = if p.top { (Vec2::new(x, -h - 2.0), HAlign::Right) } else { (Vec2::new(x, 2.0), HAlign::Left) };
                kinds.push(att(&format!("PTDESC{n:02}"), at, 2.0, "", false, al, true));
            }
        } else {
            kinds.push(att(&format!("ADDR{n:02}"), Vec2::new(x, -h / 2.0), 1.5, "", true, HAlign::Center, true));
        }
    }
    // The title column left of the box, reading upwards: location, manufacturer, catalogue
    // number, the two rack/slot lines, the description and the tag.
    let y = -h;
    let col = |i: f64| Vec2::new(-1.6 - 2.5 * i, y);
    kinds.push(att(if child { "TAG2" } else { "TAG1" }, col(0.0), 2.2, "", false, HAlign::Left, true));
    kinds.push(att("DESC1", col(1.0), 1.8, "", false, HAlign::Left, true));
    kinds.push(att("LINE2", col(2.0), 1.8, "", false, HAlign::Left, true));
    kinds.push(att("LINE1", col(3.0), 1.8, "", false, HAlign::Left, true));
    kinds.push(att("CAT", col(4.0), 1.8, &part.cat, false, HAlign::Left, true));
    kinds.push(att("MFG", col(5.0), 1.8, &part.mfg, false, HAlign::Left, true));
    kinds.push(att("LOC", col(6.0), 1.8, "", false, HAlign::Left, true));
    // Cross-references (other groups of this part) inside the box, top left.
    kinds.push(att("XREF", Vec2::new(1.0, -h + 1.5), 1.8, "", false, HAlign::Left, false));
    let hidden = Vec2::new(1.0, -h / 2.0);
    for (tag, value) in [
        ("DESC2", ""),
        ("DESC3", ""),
        ("INST", ""),
        ("RATING", part.rating.as_str()),
        ("ECLASS", part.class.as_str()),
        ("EFAMILY", part.family.as_str()),
        ("EPART", part.cat.as_str()),
        ("EID", ""),
    ] {
        kinds.push(att(tag, hidden, 1.0, value, true, HAlign::Left, false));
    }
    let mut b = Block::new(name);
    b.description = format!("{} {} {}", part.mfg, part.cat, part.description).trim().to_string();
    for k in kinds {
        b.entities.push(Entity { handle: new_handle(), common: Common::default(), kind: k });
    }
    b
}

/// The block's name: `PLC_5069_IB16`, `VFD_25B_D010N114_CONTROL`, `…_C` for a child part.
fn block_name(part: &Part, groups: &[usize], child: bool, pitch: Option<f64>) -> String {
    let clean = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' }).collect::<String>();
    let mut n = format!("{}_{}", if part.family == "VFD" { "VFD" } else { "PLC" }, clean(&part.cat));
    if groups.len() < part.groups.len() {
        for g in groups {
            if let Some(gr) = part.groups.get(*g) {
                n.push('_');
                n.push_str(&clean(&gr.name));
            }
        }
    }
    if let Some(p) = pitch {
        n.push_str(&format!("_P{}", (p * 10.0).round() as i64));
    }
    if child {
        n.push_str("_C");
    }
    n
}

// ----------------------------------------------------------------------------------------------
// The command.

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(CMD, "Insert Part", run_part)
            .menu(&["Tools", "Electrical", "Insert PLC / Drive Part..."])
            .params(
                "{cat (catalogue number, e.g. 5069-IB16, 25B-D010N114) | spec: {mfg, cat, family: PLC | VFD, class?, series?, description?, rating?, \
             formats?: {DI: \"{rack}I{slot}.{n2}\"}, starts?: {DI: 0}, groups: [{name, top: [{num, label, io?: DI|DO|AI|AO}], bottom}]}, \
             at: [x, y], group? (name, or names, to place only part of it), tag?, rack? (\"A2:\"), slot?, start? (number, or {DI, DO, AI, AO}), \
             pitch?, values? {DESC1, LINE1, LINE2, LOC, PTDESC01…}} → {handle, block, tag, addresses: [{terminal, address}], child} · \
             {list: true, mfg?, family?, search?} → {parts: [{mfg, cat, family, series, description, rating, groups, io, terminals, checked}]} · \
             {cat, info: true} → the part with its terminals",
            )
            .interactive(|_| Ok(Box::new(PartM::default()))),
    ]
}

fn group_param(part: &Part, p: &Value) -> Result<Vec<usize>> {
    let names: Vec<String> = match p.get("group") {
        None | Some(Value::Null) => return Ok((0..part.groups.len()).collect()),
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).take(MAX_GROUPS).collect(),
        Some(_) => return Err(bad(CMD, "`group` must be a name or a list of names")),
    };
    let mut out = Vec::new();
    for n in &names {
        let i = part.groups.iter().position(|g| g.name.eq_ignore_ascii_case(n.trim())).ok_or_else(|| {
            bad(
                CMD,
                format!(
                    "{} has no group \"{n}\"; its groups are {}",
                    part.cat,
                    part.groups.iter().map(|g| g.name.as_str()).collect::<Vec<_>>().join(", ")
                ),
            )
        })?;
        if !out.contains(&i) {
            out.push(i);
        }
    }
    out.sort_unstable();
    if out.is_empty() {
        return Err(bad(CMD, "`group` names no group"));
    }
    Ok(out)
}

fn addr_opts(p: &Value) -> Result<AddrOpts> {
    let short = |key: &str| -> Result<String> {
        let v = p.get(key).map(value_text).unwrap_or_default();
        if v.chars().count() > 16 {
            return Err(bad(CMD, format!("`{key}` is too long")));
        }
        Ok(v.trim().to_string())
    };
    let mut o = AddrOpts { rack: short("rack")?, slot: short("slot")?, ..AddrOpts::default() };
    match p.get("start") {
        None | Some(Value::Null) => {}
        Some(Value::Object(m)) => {
            for (k, v) in m {
                let kind = Io::parse(k).ok_or_else(|| bad(CMD, format!("`start.{k}`: kinds are DI, DO, AI, AO")))?;
                if let Some(s) = o.starts.get_mut(kind.index()) {
                    *s = Some(clip(&value_text(v)));
                }
            }
        }
        Some(v @ (Value::String(_) | Value::Number(_))) => {
            let s = clip(&value_text(v));
            o.starts = [Some(s.clone()), Some(s.clone()), Some(s.clone()), Some(s)];
        }
        Some(_) => return Err(bad(CMD, "`start` must be a number or {DI, DO, AI, AO}")),
    }
    if let Some(m) = p.get("formats").and_then(Value::as_object) {
        for (k, v) in m {
            let kind = Io::parse(k).ok_or_else(|| bad(CMD, format!("`formats.{k}`: kinds are DI, DO, AI, AO")))?;
            if let (Some(s), Some(f)) = (o.formats.get_mut(kind.index()), v.as_str()) {
                *s = Some(clip(f));
            }
        }
    }
    Ok(o)
}

fn part_param(p: &Value) -> Result<Part> {
    if let Some(sp) = p.get("spec").filter(|v| !v.is_null()) {
        return part_from_spec(sp);
    }
    let cat =
        str_param(p, "cat").map(str::trim).filter(|c| !c.is_empty()).ok_or_else(|| bad(CMD, "`cat` (a catalogue number) or `spec` is required"))?;
    find(cat).cloned().ok_or_else(|| EngineError::Other(format!("No part \"{cat}\" in the catalogue; {{\"list\": true}} lists them.")))
}

fn part_info(part: &Part) -> Value {
    let groups: Vec<Value> = part
        .groups
        .iter()
        .map(|g| {
            let side = |ts: &[Term]| {
                ts.iter()
                    .filter(|x| !x.num.is_empty())
                    .map(|x| json!({ "num": x.num, "label": x.label, "io": x.io.map(Io::name) }))
                    .collect::<Vec<_>>()
            };
            json!({ "name": g.name, "top": side(&g.top), "bottom": side(&g.bottom) })
        })
        .collect();
    let mut v = part.summary();
    v["groups"] = json!(groups);
    v["formats"] = json!(Io::ALL.iter().map(|k| (k.name().to_string(), json!(part.formats.get(k.index())))).collect::<Map<_, _>>());
    v["starts"] = json!(Io::ALL.iter().map(|k| (k.name().to_string(), json!(part.starts.get(k.index())))).collect::<Map<_, _>>());
    v
}

fn run_list(p: &Value) -> Value {
    let want = |key: &str| str_param(p, key).map(|s| s.trim().to_ascii_uppercase()).filter(|s| !s.is_empty());
    let (mfg, family, search) = (want("mfg"), want("family"), want("search"));
    let parts: Vec<Value> = library()
        .iter()
        .filter(|x| mfg.as_ref().is_none_or(|m| x.mfg.to_ascii_uppercase().contains(m.as_str())))
        .filter(|x| family.as_ref().is_none_or(|f| x.family.eq_ignore_ascii_case(f)))
        .filter(|x| {
            search.as_ref().is_none_or(|s| {
                let hay = format!("{} {} {} {} {}", x.mfg, x.cat, x.series, x.description, x.rating).to_ascii_uppercase();
                s.split_whitespace().all(|w| hay.contains(w))
            })
        })
        .map(Part::summary)
        .collect();
    json!({ "parts": parts })
}

/// Whether a main device tagged `tag` is in the current space.
fn main_exists(s: &Session, tag: &str) -> bool {
    let Ok(d) = s.doc() else { return false };
    let Some(store) = d.space(&s.space()) else { return false };
    store
        .iter()
        .any(|e| matches!(&e.kind, EntityKind::Insert(i) if i.attribs.iter().any(|a| a.tag.eq_ignore_ascii_case("TAG1") && a.text.value == tag)))
}

pub(super) fn run_part(s: &mut Session, p: &Value) -> Result<Value> {
    if p.get("list").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(run_list(p));
    }
    let part = part_param(p)?;
    if p.get("info").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(part_info(&part));
    }
    let at = point_req(CMD, p, "at")?;
    let groups = group_param(&part, p)?;
    let opts = addr_opts(p)?;
    let pitch = match p.get("pitch") {
        None | Some(Value::Null) => None,
        Some(_) => {
            let v = f64_or(p, "pitch", f64::NAN);
            if !(v.is_finite() && (2.0..=100.0).contains(&v)) {
                return Err(bad(CMD, "`pitch` must be 2 to 100"));
            }
            Some(v)
        }
    };
    let tag = str_param(p, "tag").map(str::trim).filter(|t| !t.is_empty()).map(str::to_string);
    if tag.as_ref().is_some_and(|t| t.chars().count() > 64) {
        return Err(bad(CMD, "`tag` is too long"));
    }
    // A group placed after the part's first: it follows the main part's tag.
    let child = match p.get("child").and_then(Value::as_bool) {
        Some(c) => c && tag.is_some(),
        None => groups.len() < part.groups.len() && tag.as_ref().is_some_and(|t| main_exists(s, t)),
    };
    let name = block_name(&part, &groups, child, pitch);
    if s.doc()?.block(&name).is_none() {
        let d = s.doc_mut()?;
        let b = build_block(&part, &groups, child, pitch, || d.new_handle(), &name);
        d.blocks.insert(name.clone(), Arc::new(b));
    }
    // Attribute values: addresses, the title lines, the caller's values.
    let addrs = addresses(&part, &opts);
    let (placed, _) = layout(&part, &groups);
    let mut values = Map::new();
    let mut list = Vec::new();
    for pl in &placed {
        let a = addrs.get(pl.index).cloned().unwrap_or_default();
        if !a.is_empty() {
            values.insert(format!("ADDR{:02}", pl.index + 1), json!(a));
        }
        list.push(json!({ "terminal": pl.term.num, "label": pl.term.label, "address": a }));
    }
    values.insert("DESC1".into(), json!(part.description));
    let line1 = if part.family == "VFD" {
        part.rating.clone()
    } else if opts.slot.is_empty() {
        String::new()
    } else {
        format!("MODULE {}", opts.slot)
    };
    values.insert("LINE1".into(), json!(line1));
    values.insert("LINE2".into(), json!(part.series));
    if let Some(m) = p.get("values").and_then(Value::as_object) {
        for (k, v) in m.iter().take(512) {
            let key = k.trim().to_ascii_uppercase();
            if key.is_empty() || key.len() > 32 || key.starts_with("TAG") || key.starts_with("TERM") || key.starts_with('X') {
                continue;
            }
            values.insert(key, json!(clip(&value_text(v))));
        }
    }
    let mut params = json!({ "name": name, "at": [at.x, at.y], "values": values, "breakWires": false });
    if let Some(t) = &tag {
        params["tag"] = json!(t);
    }
    let r = super::symbols::run_esymbol(s, &params)?;
    Ok(json!({ "handle": r["handle"], "block": name, "tag": r["tag"], "addresses": list, "child": child, "checked": part.checked }))
}

/// EPART at the command line: the catalogue number, then where it goes.
#[derive(Default)]
pub(super) struct PartM {
    cat: Option<String>,
}

impl Interactive for PartM {
    fn name(&self) -> &'static str {
        "EPART"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match &self.cat {
            None => Prompt::new("Catalogue number or [?]", Accept { point: false, number: false, text: true, select: false, enter: false }),
            Some(c) => {
                Prompt::new(format!("Insertion point for {c}"), Accept { point: true, number: false, text: false, select: false, enter: false })
            }
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match (&self.cat, i) {
            (None, Input::Text(t) | Input::Keyword(t)) if t.trim() == "?" => {
                let names: Vec<String> = library().iter().map(|p| format!("{} ({})", p.cat, p.description)).collect();
                s.echo(format!("Parts: {}", names.join(", ")));
                Ok(Step::Continue)
            }
            (None, Input::Text(t) | Input::Keyword(t)) => {
                let part = find(t.trim()).ok_or_else(|| EngineError::Other(format!("No part \"{}\"; type ? for the list.", t.trim())))?;
                self.cat = Some(part.cat.clone());
                Ok(Step::Continue)
            }
            (Some(c), Input::Point(p)) => {
                let c = c.clone();
                let r = run_part(s, &json!({ "cat": c, "at": [p.x, p.y] }))?;
                if let Some(h) = r["handle"].as_str() {
                    s.request_ui(json!({ "dialog": "ecomponent", "handle": h, "block": r["block"] }));
                }
                Ok(Step::Done)
            }
            _ => Ok(Step::Continue),
        }
    }
}

/// The PLC modules of the current space for the rack layout report: tag, catalogue number,
/// the two title lines, first and last address, and where it is.
pub(super) fn modules(s: &Session) -> Result<Vec<Vec<String>>> {
    let tags = super::terminals::tags_by_handle(s)?;
    let d = s.doc()?;
    let Some(store) = d.space(&s.space()) else { return Ok(Vec::new()) };
    let mut rows = Vec::new();
    for e in store.iter() {
        let EntityKind::Insert(ins) = &e.kind else { continue };
        if !ins.block.to_ascii_uppercase().starts_with("PLC_") {
            continue;
        }
        let get = |t: &str| ins.attribs.iter().find(|a| a.tag.eq_ignore_ascii_case(t)).map(|a| a.text.value.clone()).unwrap_or_default();
        let mut addrs: Vec<(String, String)> = ins
            .attribs
            .iter()
            .filter(|a| a.tag.to_ascii_uppercase().starts_with("ADDR") && !a.text.value.is_empty())
            .map(|a| (a.tag.to_ascii_uppercase(), a.text.value.clone()))
            .collect();
        addrs.sort();
        let first = addrs.first().map(|a| a.1.clone()).unwrap_or_default();
        let last = addrs.last().map(|a| a.1.clone()).unwrap_or_default();
        rows.push((
            ins.insert.x,
            vec![tags.get(&e.handle).cloned().unwrap_or_default(), get("MFG"), get("CAT"), get("LINE1"), get("LINE2"), first, last],
        ));
    }
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    Ok(rows.into_iter().map(|r| r.1).collect())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn addresses_in_each_style() {
        assert_eq!(address("{rack}I{slot}.{n2}", "0", 2, "A2:", "20"), "A2:I20.02");
        assert_eq!(address("{rack}O{slot}.{n2}", "0", 15, "", "3"), "O3.15");
        assert_eq!(address("%I{byte}.{bit}", "0", 9, "", ""), "%I1.1");
        assert_eq!(address("%I{byte}.{bit}", "8", 0, "", ""), "%I8.0");
        assert_eq!(address("%IW{word}", "64", 1, "", ""), "%IW66");
        assert_eq!(address("X{o}", "0", 8, "", ""), "X10");
        assert_eq!(address("X{o}", "20", 3, "", ""), "X23");
        assert_eq!(address("X{x}", "10", 6, "", ""), "X16");
        assert_eq!(address("", "0", 1, "", ""), "");
        assert_eq!(address("{n}", "junk", u64::MAX, "", ""), u64::MAX.to_string(), "hostile numbers saturate");
    }

    #[test]
    fn every_part_fits_and_has_unique_cat() {
        let mut seen = std::collections::HashSet::new();
        for part in library() {
            assert!(seen.insert(part.cat.clone()), "duplicate {}", part.cat);
            let n = part.terms().count();
            assert!((1..=MAX_TERMS).contains(&n), "{}: {n} terminals", part.cat);
            assert!(!part.groups.is_empty());
        }
        assert!(library().len() >= 25);
        assert!(find("5069-ib16").is_some() && find(" 25B-D010N114 ").is_some() && find("nope").is_none());
    }

    #[test]
    fn places_a_5069_input_card_like_autocad_electrical() {
        let mut s = Session::new();
        let r = s.execute("epart", &json!({ "cat": "5069-IB16", "at": [40, 60], "slot": 1 })).unwrap();
        assert_eq!(r["block"], "PLC_5069_IB16");
        assert!(r["tag"].as_str().unwrap().starts_with("-KF"), "{r}");
        let addrs = r["addresses"].as_array().unwrap();
        assert_eq!(addrs.len(), 18);
        assert_eq!(addrs[0]["address"], "I1.00");
        assert_eq!(addrs[15]["address"], "I1.15");
        assert_eq!(addrs[16]["address"], "", "NC terminals have no address");
        // A wire on its first input is numbered with the address.
        s.execute("ewire", &json!({ "from": [50, 150], "to": [50, 65] })).unwrap();
        let n = s.execute("ewirenum", &json!({})).unwrap();
        assert!(n["wires"].as_array().unwrap().iter().any(|w| w["number"] == "I1.00"), "{n}");
        // The I/O list reads it, spare terminals left out.
        let io = s.execute("ereport", &json!({ "kind": "plcio" })).unwrap();
        assert_eq!(io["rows"].as_array().unwrap().len(), 16, "{io}");
        // Remote rack, other slot.
        let r = s.execute("epart", &json!({ "cat": "1734-OB8", "at": [40, 0], "rack": "A2:", "slot": 32 })).unwrap();
        assert_eq!(r["addresses"][7]["address"], "A2:O32.07");
        let rack = s.execute("ereport", &json!({ "kind": "plcmodules" })).unwrap();
        assert_eq!(rack["rows"].as_array().unwrap().len(), 2, "{rack}");
    }

    #[test]
    fn a_drive_placed_in_two_groups_cross_references() {
        let mut s = Session::new();
        let a = s.execute("epart", &json!({ "cat": "25B-D010N114", "group": "power", "at": [50, 150] })).unwrap();
        let tag = a["tag"].as_str().unwrap().to_string();
        assert!(tag.starts_with("-TA"), "{a}");
        assert_eq!(a["child"], false);
        let b = s.execute("epart", &json!({ "cat": "25B-D010N114", "group": "control", "at": [50, 50], "tag": tag })).unwrap();
        assert_eq!(b["child"], true);
        assert_eq!(b["tag"], json!(tag));
        assert!(b["block"].as_str().unwrap().ends_with("_CONTROL_C"));
        // Siemens and Mitsubishi addresses.
        let c = s.execute("epart", &json!({ "cat": "6ES7214-1AG40-0XB0", "at": [500, 100] })).unwrap();
        let first_dq = c["addresses"].as_array().unwrap().iter().find(|a| a["terminal"] == "DQa.0").unwrap().clone();
        assert_eq!(first_dq["address"], "%Q0.0");
        let ai1 = c["addresses"].as_array().unwrap().iter().find(|a| a["terminal"] == "AI1").unwrap().clone();
        assert_eq!(ai1["address"], "%IW66");
        let f = s.execute("epart", &json!({ "cat": "FX5U-32MT/ES", "at": [900, 100] })).unwrap();
        let y17 = f["addresses"].as_array().unwrap().iter().find(|a| a["terminal"] == "Y17").unwrap().clone();
        assert_eq!(y17["address"], "Y17");
    }

    #[test]
    fn list_info_spec_and_bad_params() {
        let mut s = Session::new();
        let l = s.execute("epart", &json!({ "list": true, "family": "VFD" })).unwrap();
        assert!(l["parts"].as_array().unwrap().iter().all(|p| p["family"] == "VFD"));
        let l = s.execute("epart", &json!({ "list": true, "search": "5069 input" })).unwrap();
        assert!(!l["parts"].as_array().unwrap().is_empty());
        let i = s.execute("epart", &json!({ "cat": "25B-D2P3N114", "info": true })).unwrap();
        assert_eq!(i["groups"][1]["top"][0]["num"], "11");
        let spec = json!({ "mfg": "ACME", "cat": "IO-4", "groups": [{ "name": "io", "top": [{ "num": "1", "label": "IN 1", "io": "DI" }, { "num": "2", "label": "IN 2", "io": "DI" }] }] });
        let r = s.execute("epart", &json!({ "spec": spec, "at": [0, 0], "slot": 5 })).unwrap();
        assert_eq!(r["addresses"][1]["address"], "I5.01");
        for bad in [
            json!({ "at": [0, 0] }),
            json!({ "cat": "NOPE", "at": [0, 0] }),
            json!({ "cat": "5069-IB16" }),
            json!({ "cat": "5069-IB16", "at": [0, 0], "group": "nope" }),
            json!({ "cat": "5069-IB16", "at": [0, 0], "pitch": 1e9 }),
            json!({ "cat": "5069-IB16", "at": [0, 0], "rack": "x".repeat(100) }),
            json!({ "spec": { "cat": "X", "groups": [] }, "at": [0, 0] }),
            json!({ "spec": { "cat": "X", "family": "robot", "groups": [{ "name": "a", "top": [{ "num": "1" }] }] }, "at": [0, 0] }),
            json!({ "spec": "nonsense", "at": [0, 0] }),
        ] {
            assert!(s.execute("epart", &bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn placed_from_the_catalogue_with_a_click() {
        let mut s = Session::new();
        s.place_preset = Some(json!({ "command": "epart", "params": { "cat": "5069-L320ER", "slot": "0" }, "label": "5069-L320ER" }));
        s.start("eplace").unwrap();
        s.input(Input::Point(Vec2::new(20.0, 40.0))).unwrap();
        assert!(s.running.is_none(), "placed once");
        assert!(s.doc().unwrap().block("PLC_5069_L320ER").is_some());
    }

    #[test]
    fn command_line_form() {
        let mut s = Session::new();
        s.cmdline("epart").unwrap();
        s.cmdline("5069-OB16").unwrap();
        s.input(Input::Point(Vec2::new(10.0, 10.0))).unwrap();
        assert!(s.running.is_none());
        assert!(s.doc().unwrap().block("PLC_5069_OB16").is_some());
    }
}
