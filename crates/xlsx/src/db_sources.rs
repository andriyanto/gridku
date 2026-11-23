//! Where the tables loaded from a database came from (Data › DB Connect,
//! [`gridcraft_model::DbSource`]), kept in the file as a custom XML part (ECMA-376 Part 1, 22.5:
//! `customXml/itemN.xml` with its `itemPropsN.xml`), the place the format gives other
//! applications' data. Excel neither reads nor runs it and keeps it when it saves the workbook.
//!
//! The part's root is `gridkuDbSources` in the namespace [`NS`]; each `source` names its sheet
//! table by name (an id isn't kept by every writer) and holds the engine, where the database is,
//! the database table, its columns' declared types and how rows are told apart. A database
//! file is written as the caller gives it (the engine makes it relative to the workbook's folder
//! when saving to a file); SQLite rowids are not kept, as a refresh reads them again. No
//! password is ever written. Hostile input is bounded: the part's size, the sources and the
//! columns read.

use std::fmt::Write as _;
use std::sync::Arc;

use gridcraft_model::{DbColumn, DbEngine, DbRowKey, DbSource, Workbook};

use crate::package::Rel;
use crate::read::Ctx;
use crate::write::{Out, Rels, XML_DECL};
use crate::xml::{self, El, esc_attr};

/// Namespace of the part's root, which tells GridKu's part from other custom XML.
pub const NS: &str = "urn:gridku:db-sources:1";
/// The root element's name.
const ROOT: &str = "gridkuDbSources";
/// The custom XML properties part's content type.
const CT_PROPS: &str = "application/vnd.openxmlformats-officedocument.customXmlProperties+xml";
const NS_DATASTORE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/customXml";
/// The item's id in its properties part: fixed, so the part is the same in every file.
const ITEM_ID: &str = "{5E1F3A2B-7C4D-4E8F-9A1B-2C3D4E5F6A7B}";

/// Largest part read.
const MAX_PART_BYTES: u64 = 4 * 1024 * 1024;
/// Most sources read.
const MAX_SOURCES: usize = 1024;
/// Most columns read per source.
const MAX_COLUMNS: usize = 16_384;

/// The sources of `wb` whose sheet table is still there, with that table's name.
fn live(wb: &Workbook) -> Vec<(&DbSource, String)> {
    wb.db_sources
        .iter()
        .filter_map(|s| {
            let name = wb.sheets.iter().flat_map(|sh| &sh.tables).find(|t| t.id == s.table_id)?.name.clone();
            Some((s.as_ref(), name))
        })
        .collect()
}

fn source_xml(s: &DbSource, table: &str, x: &mut String) {
    let _ = write!(
        x,
        "<source engine=\"{}\" location=\"{}\" table=\"{}\" sheetTable=\"{}\"",
        s.engine.id(),
        esc_attr(&s.location),
        esc_attr(&s.table),
        esc_attr(table)
    );
    if let Some(schema) = &s.schema {
        let _ = write!(x, " schema=\"{}\"", esc_attr(schema));
    }
    x.push('>');
    for c in &s.columns {
        let _ = write!(x, "<column name=\"{}\" type=\"{}\"", esc_attr(&c.name), esc_attr(&c.data_type));
        if let Some(l) = &c.length {
            let _ = write!(x, " length=\"{}\"", esc_attr(l));
        }
        let _ = write!(x, " nullable=\"{}\" primaryKey=\"{}\"/>", u8::from(c.nullable), c.primary_key);
    }
    match &s.key {
        DbRowKey::Columns(cols) => {
            let list: Vec<String> = cols.iter().map(usize::to_string).collect();
            let _ = write!(x, "<key kind=\"columns\" columns=\"{}\"/>", list.join(" "));
        }
        DbRowKey::Rowid(_) => x.push_str("<key kind=\"rowid\"/>"),
        DbRowKey::None => x.push_str("<key kind=\"none\"/>"),
    }
    x.push_str("</source>");
}

/// Writes the part when the workbook has loaded tables, related from the workbook.
pub(crate) fn write(wb: &Workbook, wb_rels: &mut Rels, out: &mut Out) {
    let sources = live(wb);
    if sources.is_empty() {
        return;
    }
    let mut x = format!("{XML_DECL}<{ROOT} xmlns=\"{NS}\">");
    for (s, table) in &sources {
        source_xml(s, table, &mut x);
    }
    let _ = write!(x, "</{ROOT}>");
    // The first item number no kept custom XML part has.
    let n = (1u32..)
        .find(|n| out.kept.reserve(&format!("customXml/item{n}.xml")) && out.kept.reserve(&format!("customXml/itemProps{n}.xml")))
        .unwrap_or(1);
    let item = format!("customXml/item{n}.xml");
    let props = format!("customXml/itemProps{n}.xml");
    // The item takes the package's default `xml` content type, as Excel writes custom XML.
    out.part(&item, None, x.into_bytes());
    let p = format!(
        "{XML_DECL}<ds:datastoreItem ds:itemID=\"{ITEM_ID}\" xmlns:ds=\"{NS_DATASTORE}\"><ds:schemaRefs><ds:schemaRef ds:uri=\"{NS}\"/></ds:schemaRefs></ds:datastoreItem>"
    );
    out.part(&props, Some(CT_PROPS), p.into_bytes());
    let mut rels = Rels::default();
    rels.add("customXmlProps", &format!("itemProps{n}.xml"));
    out.part(&format!("customXml/_rels/item{n}.xml.rels"), None, rels.xml().into_bytes());
    wb_rels.add("customXml", &format!("../{item}"));
}

/// The custom XML part of the workbook's relationships that is GridKu's, if any.
fn ours(cx: &mut Ctx<'_>, wb_rels: &[Rel]) -> Option<(String, Vec<u8>)> {
    for r in wb_rels.iter().filter(|r| !r.external && r.kind.eq_ignore_ascii_case("customXml")) {
        let Ok(Some(bytes)) = cx.pkg.read_limited(&r.target, MAX_PART_BYTES) else { continue };
        let head = String::from_utf8_lossy(bytes.get(..bytes.len().min(512)).unwrap_or_default()).into_owned();
        if head.contains(NS) && head.contains(ROOT) {
            return Some((r.target.clone(), bytes));
        }
    }
    None
}

fn column(c: &El) -> DbColumn {
    DbColumn {
        name: c.attr("name").unwrap_or("").to_string(),
        data_type: c.attr("type").unwrap_or("").to_string(),
        length: c.attr("length").map(str::to_string),
        nullable: c.flag("nullable", true),
        primary_key: c.attr_u32("primaryKey").unwrap_or(0),
    }
}

/// Reads GridKu's part into `wb.db_sources` (after the sheets, whose tables the sources name),
/// and returns its part name so that it isn't also kept as unmodelled content.
pub(crate) fn read(cx: &mut Ctx<'_>, wb_rels: &[Rel], wb: &mut Workbook) -> Option<String> {
    let (name, bytes) = ours(cx, wb_rels)?;
    let root = match xml::parse(&bytes) {
        Ok(root) if root.name == ROOT => root,
        _ => {
            cx.warn("where the tables loaded from a database came from could not be read");
            return Some(name);
        }
    };
    for s in root.kids("source").take(MAX_SOURCES) {
        let (Some(engine), Some(location), Some(table), Some(sheet_table)) =
            (s.attr("engine").and_then(DbEngine::parse), s.attr("location"), s.attr("table"), s.attr("sheetTable"))
        else {
            continue;
        };
        let Some(id) = wb.sheets.iter().flat_map(|sh| &sh.tables).find(|t| t.name.eq_ignore_ascii_case(sheet_table)).map(|t| t.id) else { continue };
        let columns: Vec<DbColumn> = s.kids("column").take(MAX_COLUMNS).map(column).collect();
        let key = match s.child("key") {
            Some(k) if k.attr("kind") == Some("columns") => DbRowKey::Columns(
                k.attr("columns").unwrap_or("").split_whitespace().filter_map(|c| c.parse::<usize>().ok()).filter(|c| *c < columns.len()).collect(),
            ),
            Some(k) if k.attr("kind") == Some("rowid") => DbRowKey::Rowid(vec![]),
            _ => DbRowKey::None,
        };
        wb.db_sources.retain(|o| o.table_id != id);
        wb.db_sources.push(Arc::new(DbSource {
            engine,
            location: location.to_string(),
            schema: s.attr("schema").map(str::to_string),
            table: table.to_string(),
            table_id: id,
            columns,
            key,
        }));
    }
    Some(name)
}
