//! Command-line forms of the electrical commands that take points and options: ELADDER (two
//! corners), ECABLE (two points across the wires), EPLC (kind, points, insertion point) and
//! EWIRETYPE (colour, cross-section). Each runs the JSON form when it has what it needs.

use cadcraft_geom::Vec2;
use serde_json::json;

use crate::cmd::curves::KW;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

const POINT: Accept = Accept { point: true, number: false, text: false, select: false, enter: false };
const NUMBER: Accept = Accept { point: false, number: true, text: true, select: false, enter: true };

fn number(i: &Input) -> Option<f64> {
    match i {
        Input::Text(t) | Input::Keyword(t) => t.trim().replace(',', ".").parse().ok().filter(|v: &f64| v.is_finite()),
        _ => None,
    }
}

/// ELADDER: the top-left and bottom-right corners of the ladder.
#[derive(Default)]
pub(super) struct LadderM {
    first: Option<Vec2>,
}

impl Interactive for LadderM {
    fn name(&self) -> &'static str {
        "ELADDER"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.first.is_none() { Prompt::new("Top rail: left end", POINT) } else { Prompt::new("Bottom rail: right end", POINT) }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let Input::Point(p) = i else { return Ok(if matches!(i, Input::Enter | Input::Cancel) { Step::Done } else { Step::Continue }) };
        match self.first {
            None => {
                self.first = Some(p);
                Ok(Step::Continue)
            }
            Some(a) => {
                s.execute("eladder", &json!({ "top": a.y.max(p.y), "bottom": a.y.min(p.y), "from": a.x.min(p.x), "to": a.x.max(p.x) }))?;
                Ok(Step::Done)
            }
        }
    }
}

/// ECABLE: two points of a line drawn across the cable's wires.
#[derive(Default)]
pub(super) struct CableM {
    first: Option<Vec2>,
}

impl Interactive for CableM {
    fn name(&self) -> &'static str {
        "ECABLE"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.first.is_none() {
            Prompt::new("Cable line: first point (across the wires)", POINT)
        } else {
            Prompt::new("Cable line: second point", POINT)
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let Input::Point(p) = i else { return Ok(if matches!(i, Input::Enter | Input::Cancel) { Step::Done } else { Step::Continue }) };
        match self.first {
            None => {
                self.first = Some(p);
                Ok(Step::Continue)
            }
            Some(a) => {
                let r = s.execute("ecable", &json!({ "from": [a.x, a.y], "to": [p.x, p.y] }))?;
                let n = r["cores"].as_array().map_or(0, Vec::len);
                s.echo(format!("Cable {} carries {n} core(s); set its type in its CABLETYPE attribute.", r["tag"].as_str().unwrap_or("")));
                Ok(Step::Done)
            }
        }
    }
}

/// EPLC: kind, number of points, insertion point.
pub(super) struct PlcM {
    kind: Option<String>,
    points: Option<u64>,
}

impl PlcM {
    pub(super) fn new() -> Self {
        PlcM { kind: None, points: None }
    }
}

impl Interactive for PlcM {
    fn name(&self) -> &'static str {
        "EPLC"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match (&self.kind, self.points) {
            (None, _) => Prompt::new("Card", KW).kw(&["DI", "DO", "AI", "AO"]).default("DI"),
            (Some(_), None) => Prompt::new("Number of points", NUMBER).default("8"),
            _ => Prompt::new("Insertion point", POINT),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if self.kind.is_none() {
            self.kind = Some(match &i {
                Input::Enter => "DI".into(),
                Input::Keyword(k) | Input::Text(k) if ["DI", "DO", "AI", "AO"].contains(&k.trim().to_ascii_uppercase().as_str()) => {
                    k.trim().to_ascii_uppercase()
                }
                Input::Cancel => return Ok(Step::Done),
                Input::Keyword(_) | Input::Text(_) => return Err(EngineError::Other("Type DI, DO, AI or AO.".into())),
                _ => return Ok(Step::Continue),
            });
            return Ok(Step::Continue);
        }
        if self.points.is_none() {
            self.points = Some(match &i {
                Input::Enter => 8,
                Input::Cancel => return Ok(Step::Done),
                Input::Point(_) | Input::Pick(_) | Input::Deferred(_) => return Ok(Step::Continue),
                other => number(other)
                    .filter(|n| (1.0..=64.0).contains(n))
                    .map(|n| n as u64)
                    .ok_or_else(|| EngineError::Other("Type a number of points from 1 to 64.".into()))?,
            });
            return Ok(Step::Continue);
        }
        let Input::Point(p) = i else { return Ok(Step::Continue) };
        s.execute("eplc", &json!({ "kind": self.kind, "points": self.points, "at": [p.x, p.y] }))?;
        Ok(Step::Done)
    }
}

/// EWIRETYPE: colour, then cross-section; the wire type becomes current.
#[derive(Default)]
pub(super) struct WireTypeM {
    colour: Option<String>,
}

impl Interactive for WireTypeM {
    fn name(&self) -> &'static str {
        "EWIRETYPE"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match &self.colour {
            None => {
                Prompt::new("Wire colour", KW).kw(&["BK", "BN", "RD", "OG", "YE", "GN", "BU", "VT", "GY", "WH", "PK", "GNYE", "DBU"]).default("BK")
            }
            Some(_) => Prompt::new("Cross-section (mm²)", NUMBER).default("1"),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match &self.colour {
            None => {
                self.colour = Some(match &i {
                    Input::Enter => "BK".into(),
                    Input::Keyword(k) | Input::Text(k) => k.trim().to_ascii_uppercase(),
                    Input::Cancel => return Ok(Step::Done),
                    _ => return Ok(Step::Continue),
                });
                Ok(Step::Continue)
            }
            Some(c) => {
                let size = match &i {
                    Input::Enter => 1.0,
                    Input::Cancel => return Ok(Step::Done),
                    Input::Point(_) | Input::Pick(_) | Input::Deferred(_) => return Ok(Step::Continue),
                    other => number(other).ok_or_else(|| EngineError::Other("Type a cross-section in mm², such as 1.5.".into()))?,
                };
                s.execute("ewiretype", &json!({ "color": c, "size": size }))?;
                Ok(Step::Done)
            }
        }
    }
}

/// Commands EPLACE may place.
pub(super) const PLACEABLE: &[&str] = &["esymbol", "eplc", "epart"];

/// EPLACE at the command line: the insertion point for what a dialog prepared
/// ([`Session::place_preset`]); each click places one more, Enter ends.
pub(super) struct PlaceM {
    preset: Option<serde_json::Value>,
    placed: usize,
}

impl PlaceM {
    pub(super) fn new(s: &Session) -> Self {
        PlaceM { preset: s.place_preset.clone(), placed: 0 }
    }
}

impl Interactive for PlaceM {
    fn name(&self) -> &'static str {
        "EPLACE"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        let what = self.preset.as_ref().and_then(|p| p["label"].as_str().or(p["command"].as_str())).unwrap_or("object").to_string();
        let accept = Accept { point: true, number: false, text: false, select: false, enter: true };
        if self.placed == 0 {
            Prompt::new(format!("Insertion point for {what}"), accept)
        } else {
            Prompt::new(format!("Next {what} or <done>"), accept)
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let Some(preset) = self.preset.clone() else {
            return Err(EngineError::Other("Nothing to place: choose it in its dialog first.".into()));
        };
        match i {
            Input::Point(p) => {
                let cmd = preset["command"].as_str().unwrap_or("");
                if !PLACEABLE.contains(&cmd) {
                    return Err(EngineError::Other(format!("EPLACE cannot place {cmd}.")));
                }
                let mut params = preset["params"].clone();
                if !params.is_object() {
                    params = json!({});
                }
                params["at"] = json!([p.x, p.y]);
                let r = match cmd {
                    "eplc" => super::plc::run_plc(s, &params)?,
                    "epart" => super::parts::run_part(s, &params)?,
                    _ => super::symbols::run_esymbol(s, &params)?,
                };
                if let Some(h) = r["handle"].as_str()
                    && cmd == "esymbol"
                {
                    s.request_ui(json!({ "dialog": "ecomponent", "handle": h }));
                }
                self.placed += 1;
                // A PLC card or a manufacturer part is placed once; symbols repeat until Enter.
                if cmd == "eplc" || cmd == "epart" {
                    s.place_preset = None;
                    return Ok(Step::Done);
                }
                Ok(Step::Continue)
            }
            Input::Enter | Input::Cancel => {
                s.place_preset = None;
                Ok(Step::Done)
            }
            _ => Ok(Step::Continue),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_forms() {
        let mut s = Session::new();
        s.start("eladder").unwrap();
        s.input(Input::Point(Vec2::new(0.0, 240.0))).unwrap();
        s.input(Input::Point(Vec2::new(210.0, 40.0))).unwrap();
        assert!(s.running.is_none());
        s.start("ewiretype").unwrap();
        s.input(Input::Keyword("BU".into())).unwrap();
        s.input(Input::Text("1.5".into())).unwrap();
        assert_eq!(s.doc().unwrap().header.str("CLAYER", ""), "WIRE_BU_1.5");
        s.start("eplc").unwrap();
        s.input(Input::Keyword("DO".into())).unwrap();
        s.input(Input::Text("4".into())).unwrap();
        s.input(Input::Point(Vec2::new(300.0, 100.0))).unwrap();
        assert!(s.doc().unwrap().block("PLC_DO4").is_some());
        s.start("ecable").unwrap();
        s.input(Input::Point(Vec2::new(10.0, 150.0))).unwrap();
        s.input(Input::Point(Vec2::new(60.0, 150.0))).unwrap();
        assert!(s.running.is_none());
        // A wrong keyword is reported and the prompt stays.
        s.start("eplc").unwrap();
        let _ = s.input(Input::Keyword("ZZ".into()));
        assert!(s.running.is_some());
        s.cancel();
    }
}
