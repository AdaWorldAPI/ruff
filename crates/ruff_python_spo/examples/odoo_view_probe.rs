//! Dev probe: run the Odoo view-XML field-set harvest over a real addon.
//! Usage: `cargo run -p ruff_python_spo --example odoo_view_probe -- <addon_root>`

#![expect(
    clippy::print_stdout,
    reason = "a dev probe's stdout report IS its deliverable"
)]

use ruff_python_spo::{ViewTarget, extract_odoo_view_field_sets_with_report};
fn main() {
    let root = std::env::args().nth(1).unwrap();
    // account.move with the real field names from the odoo-rs corpus universe.
    let target = ViewTarget {
        model: "account_move".into(),
        receivers: vec![],
        fields: [
            "partner_id",
            "date",
            "amount_total",
            "amount_untaxed",
            "amount_tax",
            "invoice_date",
            "invoice_date_due",
            "journal_id",
            "currency_id",
            "state",
            "ref",
            "narration",
            "invoice_line_ids",
            "line_ids",
            "payment_state",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect(),
    };
    let (sets, report) =
        extract_odoo_view_field_sets_with_report(std::path::Path::new(&root), &[target]);
    println!(
        "xml={} view_records={} hits={}",
        report.xml_files, report.view_records, report.views_with_hits
    );
    for s in sets.iter().take(6) {
        println!(
            "  {}  fields={}/{} referenced",
            s.view,
            s.fields.len(),
            s.referenced.len()
        );
    }
}
