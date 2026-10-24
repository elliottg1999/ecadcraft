//! EDEMO: a demonstration sheet in a new drawing. An A3 sheet with the full-width title block
//! filled in, a PowerFlex 525 drive and a 5069-IB16 input card from the parts catalogue, two
//! push-buttons wired to the card's first inputs from a 24 V rail, tagged and wire-numbered (the
//! input wires take the PLC addresses). The web preview opens it with `?demo`.

use serde_json::{Map, Value, json};

use super::project::{Project, sheet_drawing};
use super::titleblock;
use crate::cmd::{CommandSpec, always};
use crate::{Result, Session};

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("edemo", "Electrical Demo Sheet", run_demo)
            .menu(&["Tools", "Electrical", "Demo Sheet"])
            .params("{} → {drawing, parts: [tag]} (a new drawing)")
            .enabled(always)
            .noundo(),
    ]
}

const FIELDS: &[(&str, &str)] = &[
    ("COMPANY1", "ACME CONTROLS"),
    ("COMPANY2", "Technical Services Ltd"),
    ("COMPANY3", "1 Example Street, Example Park"),
    ("COMPANY4", "Heywood, Lancs."),
    ("COMPANY5", "Tel. 01234 567890"),
    ("DESIGNEDFOR1", "Example Refrigeration Services"),
    ("CUSTOMER1", "DEMO FOODS"),
    ("CUSTOMER2", "BURNLEY"),
    ("TITLE", "BATTER FEED SYSTEM"),
    ("JOBNO", "7477"),
    ("REV", "A"),
    ("DRAWN", "E.G"),
    ("DATE", "24/10/26"),
];

fn run_demo(s: &mut Session, _p: &Value) -> Result<Value> {
    let mut pr = Project { name: "ECADCraft demo".into(), title_block: titleblock::TITLE_A3.into(), ..Project::default() };
    for (k, v) in FIELDS.iter().chain(titleblock::template_defaults(titleblock::TITLE_A3)) {
        pr.properties.entry((*k).to_string()).or_insert_with(|| (*v).to_string());
    }
    let index = s.open_drawing(sheet_drawing(&pr, 1, 1), "ECAD demo", None);
    let mut sheet = Map::new();
    sheet.insert("DESCRIPTION".into(), json!("DRIVE AND DIGITAL INPUTS"));
    sheet.insert("LOC".into(), json!("A1"));
    titleblock::set_fields(s, &sheet)?;
    titleblock::write_project_fields(s, &pr.name, &pr.properties, 1, 1, "DEMO-01.dxf")?;

    let mut parts = Vec::new();
    let drive = s.execute("epart", &json!({ "cat": "25B-D010N114", "at": [40, 255] }))?;
    parts.push(drive["tag"].clone());
    let card = s.execute("epart", &json!({ "cat": "5069-IB16", "at": [40, 95], "slot": 1 }))?;
    parts.push(card["tag"].clone());
    // A 24 V rail, and two push-buttons wired from it to inputs 0 and 1 (x 50 and 70).
    s.execute("ewire", &json!({ "from": [30, 170], "to": [400, 170] }))?;
    for x in [50.0, 70.0] {
        let pb = s.execute("esymbol", &json!({ "name": "IEC_PB_NO", "at": [x, 140] }))?;
        parts.push(pb["tag"].clone());
        s.execute("ewire", &json!({ "from": [x, 170], "to": [x, 145] }))?;
        s.execute("ewire", &json!({ "from": [x, 135], "to": [x, 100] }))?;
    }
    s.execute("ewirenum", &json!({}))?;
    let _ = s.execute("zoom", &json!({ "extents": true }));
    Ok(json!({ "drawing": index, "parts": parts }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::Session;

    #[test]
    fn the_demo_sheet_has_parts_title_block_and_addresses() {
        let mut s = Session::new();
        let r = s.execute("edemo", &json!({})).unwrap();
        assert_eq!(r["parts"].as_array().unwrap().len(), 4, "{r}");
        let tb = super::titleblock::values(&s).unwrap().unwrap();
        assert_eq!(tb["DRAWINGREF"], "7477 / DEMO-01 - A");
        assert_eq!(tb["DESCRIPTION"], "DRIVE AND DIGITAL INPUTS");
        let n = s.execute("ewirenum", &json!({ "dryRun": true })).unwrap();
        let numbers: Vec<&str> = n["wires"].as_array().unwrap().iter().filter_map(|w| w["number"].as_str()).collect();
        assert!(numbers.contains(&"I1.00") && numbers.contains(&"I1.01"), "{numbers:?}");
    }
}
